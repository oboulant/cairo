use std::any::Any;
use std::collections::HashMap;
use std::str::FromStr;

use ark_ff::fields::{Fp256, MontBackend, MontConfig};
use ark_ff::{Field, PrimeField};
use ark_std::UniformRand;
use cairo_lang_utils::extract_matches;
use cairo_vm::hint_processor::hint_processor_definition::{HintProcessor, HintReference};
use cairo_vm::serde::deserialize_program::{
    ApTracking, FlowTrackingData, HintParams, ReferenceManager,
};
use cairo_vm::types::exec_scope::ExecutionScopes;
use cairo_vm::types::program::Program;
use cairo_vm::types::relocatable::{MaybeRelocatable, Relocatable};
use cairo_vm::vm::errors::vm_errors::VirtualMachineError;
use cairo_vm::vm::runners::cairo_runner::CairoRunner;
use cairo_vm::vm::vm_core::VirtualMachine;
use num_bigint::{BigInt, BigUint};
use num_traits::identities::Zero;

use crate::hints::Hint;
use crate::instructions::Instruction;
use crate::operand::{BinOpOperand, CellRef, DerefOrImmediate, Register, ResOperand};

#[cfg(test)]
#[path = "run_test.rs"]
mod test;

/// Returns the Starkware prime 2^251 + 17*2^192 + 1.
fn get_prime() -> BigInt {
    (BigInt::from(1) << 251) + 17 * (BigInt::from(1) << 192) + 1
}

// TODO(orizi): This def is duplicated.
/// Returns the Beta value of the Starkware elliptic curve.
fn get_beta() -> BigInt {
    BigInt::from_str("3141592653589793238462643383279502884197169399375105820974944592307816406665")
        .unwrap()
}

#[derive(MontConfig)]
#[modulus = "3618502788666131213697322783095070105623107215331596699973092056135872020481"]
#[generator = "3"]
struct FqConfig;
type Fq = Fp256<MontBackend<FqConfig, 4>>;

/// Convert a Hint to the cairo-vm class HintParams by canonically serializing it to a string.
fn hint_to_hint_params(hint: &Hint) -> HintParams {
    HintParams {
        code: hint.to_string(),
        accessible_scopes: vec![],
        flow_tracking_data: FlowTrackingData {
            ap_tracking: ApTracking::new(),
            reference_ids: HashMap::new(),
        },
    }
}

/// HintProcessor for Cairo compiler hints.
struct CairoHintProcessor {
    // A dict from instruction offset to hint vector.
    pub hints_dict: HashMap<usize, Vec<HintParams>>,
    // A mapping from a string that represents a hint to the hint object.
    pub string_to_hint: HashMap<String, Hint>,
}

impl CairoHintProcessor {
    pub fn new<'a, Instructions: Iterator<Item = &'a Instruction> + Clone>(
        instructions: Instructions,
    ) -> Self {
        let mut hints_dict: HashMap<usize, Vec<HintParams>> = HashMap::new();
        let mut string_to_hint: HashMap<String, Hint> = HashMap::new();

        let mut hint_offset = 0;

        for instruction in instructions {
            if !instruction.hints.is_empty() {
                // Register hint with string for the hint processor.
                for hint in instruction.hints.iter() {
                    string_to_hint.insert(hint.to_string(), hint.clone());
                }
                // Add hint, associated with the instruction offset.
                hints_dict.insert(
                    hint_offset,
                    instruction.hints.iter().map(hint_to_hint_params).collect(),
                );
            }
            hint_offset += instruction.body.op_size();
        }
        CairoHintProcessor { hints_dict, string_to_hint }
    }
}

fn cell_ref_to_relocatable(cell_ref: &CellRef, vm: &VirtualMachine) -> Relocatable {
    let base = match cell_ref.register {
        Register::AP => vm.get_ap(),
        Register::FP => vm.get_fp(),
    };
    base + (cell_ref.offset as i32)
}

/// Execution scope for starknet related data.
struct StarknetExecScope {
    /// The values of addresses in the simulated storage.
    storage: HashMap<BigInt, BigInt>,
}

impl HintProcessor for CairoHintProcessor {
    /// Trait function to execute a given hint in the hint processor.
    fn execute_hint(
        &mut self,
        vm: &mut VirtualMachine,
        exec_scopes: &mut ExecutionScopes,
        hint_data: &Box<dyn Any>,
        _constants: &HashMap<String, BigInt>,
    ) -> Result<(), VirtualMachineError> {
        let hint = hint_data.downcast_ref::<Hint>().unwrap();
        let get_cell_val = |x: &CellRef| -> Result<BigInt, VirtualMachineError> {
            Ok(vm.get_integer(&cell_ref_to_relocatable(x, vm))?.as_ref().clone())
        };
        let get_ptr =
            |cell: &CellRef, offset: &BigInt| -> Result<Relocatable, VirtualMachineError> {
                let base_ptr = vm.get_relocatable(&cell_ref_to_relocatable(cell, vm))?;
                base_ptr.add_int_mod(offset, &get_prime())
            };
        let get_double_deref_val =
            |cell: &CellRef, offset: &BigInt| -> Result<BigInt, VirtualMachineError> {
                Ok(vm.get_integer(&get_ptr(cell, offset)?)?.as_ref().clone())
            };
        let get_val = |x: &ResOperand| -> Result<BigInt, VirtualMachineError> {
            match x {
                ResOperand::Deref(cell) => get_cell_val(cell),
                ResOperand::DoubleDeref(cell, offset) => {
                    get_double_deref_val(cell, &(*offset).into())
                }
                ResOperand::Immediate(x) => Ok(x.clone()),
                ResOperand::BinOp(op) => {
                    let a = get_cell_val(&op.a)?;
                    let b = match &op.b {
                        crate::operand::DerefOrImmediate::Deref(cell) => get_cell_val(cell)?,
                        crate::operand::DerefOrImmediate::Immediate(x) => x.clone(),
                    };
                    match op.op {
                        crate::operand::Operation::Add => Ok(a + b),
                        crate::operand::Operation::Mul => Ok(a * b),
                    }
                }
            }
        };
        match hint {
            Hint::AllocSegment { dst } => {
                let segment = vm.add_memory_segment();
                vm.insert_value(&cell_ref_to_relocatable(dst, vm), segment)?;
            }
            Hint::TestLessThan { lhs, rhs, dst } => {
                let lhs_val = get_val(lhs)?;
                let rhs_val = get_val(rhs)?;
                vm.insert_value(
                    &cell_ref_to_relocatable(dst, vm),
                    if lhs_val < rhs_val { BigInt::from(1) } else { BigInt::from(0) },
                )?;
            }
            Hint::TestLessThanOrEqual { lhs, rhs, dst } => {
                let lhs_val = get_val(lhs)?;
                let rhs_val = get_val(rhs)?;
                vm.insert_value(
                    &cell_ref_to_relocatable(dst, vm),
                    if lhs_val <= rhs_val { BigInt::from(1) } else { BigInt::from(0) },
                )?;
            }
            Hint::DivMod { lhs, rhs, quotient, remainder } => {
                let lhs_val = get_val(lhs)?;
                let rhs_val = get_val(rhs)?;
                vm.insert_value(
                    &cell_ref_to_relocatable(quotient, vm),
                    lhs_val.clone() / rhs_val.clone(),
                )?;
                vm.insert_value(&cell_ref_to_relocatable(remainder, vm), lhs_val % rhs_val)?;
            }
            Hint::AllocDictFeltTo { .. } => todo!(),
            Hint::DictFeltToRead { .. } => todo!(),
            Hint::DictFeltToWrite { .. } => todo!(),
            Hint::EnterScope => todo!(),
            Hint::ExitScope => todo!(),
            Hint::DictSquashHints { .. } => todo!(),
            Hint::RandomEcPoint { x, y } => {
                // Keep sampling a random field element `X` until `X^3 + X + beta` is a quadratic
                // residue.
                let beta = Fq::from(get_beta().to_biguint().unwrap());
                let mut rng = ark_std::test_rng();
                let (random_x, random_y_squared) = loop {
                    let random_x = Fq::rand(&mut rng);
                    let random_y_squared = random_x * random_x * random_x + random_x + beta;
                    if random_y_squared.legendre().is_qr() {
                        break (random_x, random_y_squared);
                    }
                };
                let x_bigint: BigUint = random_x.into_bigint().into();
                let y_bigint: BigUint = random_y_squared.sqrt().unwrap().into_bigint().into();
                vm.insert_value(&cell_ref_to_relocatable(x, vm), BigInt::from(x_bigint))?;
                vm.insert_value(&cell_ref_to_relocatable(y, vm), BigInt::from(y_bigint))?;
            }
            Hint::SystemCall { system } => {
                let starknet_exec_scope =
                    match exec_scopes.get_mut_ref::<StarknetExecScope>("starknet_exec_scope") {
                        Ok(starknet_exec_scope) => starknet_exec_scope,
                        Err(_) => {
                            exec_scopes.assign_or_update_variable(
                                "starknet_exec_scope",
                                Box::new(StarknetExecScope { storage: HashMap::default() }),
                            );
                            exec_scopes.get_mut_ref::<StarknetExecScope>("starknet_exec_scope")?
                        }
                    };
                let (cell, base_offset) = match system {
                    ResOperand::Deref(cell) => (cell, 0.into()),
                    ResOperand::BinOp(BinOpOperand {
                        op: crate::operand::Operation::Add,
                        a,
                        b,
                    }) => (a, extract_matches!(b, DerefOrImmediate::Immediate).clone()),
                    _ => panic!("Illegal argument for system pointer."),
                };
                let (selector_sign, selector) =
                    get_double_deref_val(cell, &base_offset)?.to_bytes_be();
                assert_eq!(selector_sign, num_bigint::Sign::Plus, "Illegal selector.");
                if selector == "StorageWrite".as_bytes() {
                    let gas_counter = get_double_deref_val(cell, &(base_offset.clone() + 1))?;
                    const WRITE_GAS_SIM_COST: usize = 1000;
                    let gas_counter_updated_ptr = get_ptr(cell, &(base_offset.clone() + 5))?;
                    let revert_reason_ptr = get_ptr(cell, &(base_offset.clone() + 6))?;
                    let addr_domain = get_double_deref_val(cell, &(base_offset.clone() + 2))?;

                    // Only address_domain 0 is currently supported.
                    if addr_domain.is_zero() && gas_counter >= WRITE_GAS_SIM_COST.into() {
                        let addr = get_double_deref_val(cell, &(base_offset.clone() + 3))?;
                        let value = get_double_deref_val(cell, &(base_offset + 4))?;
                        starknet_exec_scope.storage.insert(addr, value);
                        vm.insert_value(
                            &gas_counter_updated_ptr,
                            gas_counter - WRITE_GAS_SIM_COST,
                        )?;
                        vm.insert_value(&revert_reason_ptr, BigInt::from(0))?;
                    } else {
                        vm.insert_value(&gas_counter_updated_ptr, gas_counter)?;
                        vm.insert_value(&revert_reason_ptr, BigInt::from(1))?;
                    }
                } else if selector == "StorageRead".as_bytes() {
                    let gas_counter = get_double_deref_val(cell, &(base_offset.clone() + 1))?;
                    const READ_GAS_SIM_COST: usize = 100;
                    let addr_domain = get_double_deref_val(cell, &(base_offset.clone() + 2))?;
                    let addr = get_double_deref_val(cell, &(base_offset.clone() + 3))?;

                    let gas_counter_updated_ptr = get_ptr(cell, &(base_offset.clone() + 4))?;
                    let revert_reason_ptr = get_ptr(cell, &(base_offset.clone() + 5))?;

                    // Only address_domain 0 is currently supported.
                    if addr_domain.is_zero() && gas_counter >= READ_GAS_SIM_COST.into() {
                        let value = starknet_exec_scope
                            .storage
                            .get(&addr)
                            .cloned()
                            .unwrap_or_else(|| BigInt::from(0));
                        let result_ptr = get_ptr(cell, &(base_offset + 6))?;

                        vm.insert_value(&gas_counter_updated_ptr, gas_counter - READ_GAS_SIM_COST)?;
                        vm.insert_value(&revert_reason_ptr, BigInt::from(0))?;
                        vm.insert_value(&result_ptr, value)?;
                    } else {
                        vm.insert_value(&gas_counter_updated_ptr, gas_counter)?;
                        vm.insert_value(&revert_reason_ptr, BigInt::from(1))?;
                    }
                } else if selector == "call_contract".as_bytes() {
                    todo!()
                } else {
                    panic!("Unknown selector for system call!");
                }
            }
        };
        Ok(())
    }

    /// Trait function to store hint in the hint processor by string.
    fn compile_hint(
        &self,
        hint_code: &str,
        _ap_tracking_data: &ApTracking,
        _reference_ids: &HashMap<String, usize>,
        _references: &HashMap<usize, HintReference>,
    ) -> Result<Box<dyn Any>, VirtualMachineError> {
        Ok(Box::new(self.string_to_hint[hint_code].clone()))
    }
}

/// Provides context for the `additional_initialization` callback function of [run_function].
pub struct RunFunctionContext<'a> {
    pub vm: &'a mut VirtualMachine,
    pub data_len: usize,
}

/// Runs `program` on layout with prime, and returns the memory layout and ap value.
pub fn run_function<'a, Instructions: Iterator<Item = &'a Instruction> + Clone>(
    instructions: Instructions,
    builtins: Vec<String>,
    additional_initialization: fn(
        context: RunFunctionContext<'_>,
    ) -> Result<(), Box<VirtualMachineError>>,
) -> Result<(Vec<Option<BigInt>>, usize), Box<VirtualMachineError>> {
    let data: Vec<MaybeRelocatable> = instructions
        .clone()
        .flat_map(|inst| inst.assemble().encode())
        .map(MaybeRelocatable::from)
        .collect();

    let mut hint_processor = CairoHintProcessor::new(instructions);

    let data_len = data.len();
    let program = Program {
        builtins,
        prime: get_prime(),
        data,
        constants: HashMap::new(),
        main: Some(0),
        start: None,
        end: None,
        hints: hint_processor.hints_dict.clone(),
        reference_manager: ReferenceManager { references: Vec::new() },
        identifiers: HashMap::new(),
        error_message_attributes: vec![],
        instruction_locations: None,
    };
    let mut runner = CairoRunner::new(&program, "all", false)
        .map_err(VirtualMachineError::from)
        .map_err(Box::new)?;
    let mut vm = VirtualMachine::new(get_prime(), true, vec![]);

    let end = runner.initialize(&mut vm).map_err(VirtualMachineError::from).map_err(Box::new)?;

    additional_initialization(RunFunctionContext { vm: &mut vm, data_len })?;

    runner.run_until_pc(end, &mut vm, &mut hint_processor)?;
    // TODO(alont) Remove this hack once the VM no longer squashes Nones at the end of segments.
    vm.insert_value(&vm.get_ap().add_int_mod(&1.into(), &get_prime())?, BigInt::from(0))?;
    runner.end_run(true, false, &mut vm, &mut hint_processor).map_err(Box::new)?;
    runner.relocate(&mut vm).map_err(VirtualMachineError::from).map_err(Box::new)?;
    Ok((runner.relocated_memory, runner.relocated_trace.unwrap().last().unwrap().ap))
}

/// Runs `function` and returns `n_returns` return values.
pub fn run_function_return_values<'a, Instructions: Iterator<Item = &'a Instruction> + Clone>(
    instructions: Instructions,
    builtins: Vec<String>,
    n_returns: usize,
) -> Result<Vec<BigInt>, Box<VirtualMachineError>> {
    let (cells, ap) = run_function(instructions, builtins, |_| Ok(()))?;
    // TODO(orizi): Return an error instead of unwrapping.
    let cells = cells.into_iter().skip(ap - n_returns);
    Ok(cells.take(n_returns).map(|cell| cell.unwrap()).collect())
}
