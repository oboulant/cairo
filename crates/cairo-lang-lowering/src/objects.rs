//! Intermediate representation objects after lowering from semantic.
//! This representation is SSA (static single-assignment): each variable is defined before usage and
//! assigned once. It is also normal form: each function argument is a variable, rather than a
//! compound expression.

use cairo_lang_semantic as semantic;
use cairo_lang_semantic::{ConcreteEnumId, ConcreteVariant};
use cairo_lang_utils::ordered_hash_set::OrderedHashSet;
use id_arena::Id;
use itertools::chain;
use num_bigint::BigInt;
pub mod blocks;
pub use blocks::BlockId;

pub type VariableId = Id<Variable>;

/// A block of statements. Each block gets inputs and outputs, and is composed of
/// a linear sequence of statements.
///
/// A block may end with a `return`, which exits the current function.
///
/// A block contains the list of variables to be dropped at its end. Other than these variables and
/// the output variables, it is guaranteed that no other variable is alive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StructuredBlock {
    /// Input variables to the block, including implicits.
    pub inputs: Vec<VariableId>,
    /// Statements sequence running one after the other in the block, in a linear flow.
    /// Note: Inner blocks might end with a `return`, which will exit the function in the middle.
    /// Note: Match is a possible statement, which means it has control flow logic inside, but
    /// after its execution is completed, the flow returns to the following statement of the block.
    pub statements: Vec<Statement>,
    /// Which variables are needed to be dropped at the end of this block. Note that these are
    /// not explicitly dropped by statements.
    pub drops: Vec<VariableId>,
    /// Describes how this block ends: returns to the caller or exits the function.
    pub end: StructuredBlockEnd,
}

/// Describes what happens to the program flow at the end of a [`StructuredBlock`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StructuredBlockEnd {
    /// This block returns to the call-site, outputting variables to the call-site.
    Callsite(Vec<VariableId>),
    /// This block ends with a `return` statement, exiting the function.
    Return { refs: Vec<VariableId>, returns: Vec<VariableId> },
    /// This block ends with a `panic` statement, exiting the function.
    Panic { refs: Vec<VariableId>, data: VariableId },
    /// The last statement ended the flow (e.g., match will all arms ending in return),
    /// and the end of this block is unreachable.
    Unreachable,
}

/// A block of statements. Unlike [`StructuredBlock`], this has no reference information,
/// and no panic ending.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FlatBlock {
    /// Input variables to the block, including implicits.
    pub inputs: Vec<VariableId>,
    /// Statements sequence running one after the other in the block, in a linear flow.
    /// Note: Inner blocks might end with a `return`, which will exit the function in the middle.
    /// Note: Match is a possible statement, which means it has control flow logic inside, but
    /// after its execution is completed, the flow returns to the following statement of the block.
    pub statements: Vec<Statement>,
    /// Which variables are needed to be dropped at the end of this block. Note that these are
    /// not explicitly dropped by statements.
    pub drops: Vec<VariableId>,
    /// Describes how this block ends: returns to the caller or exits the function.
    pub end: FlatBlockEnd,
}

/// Describes what happens to the program flow at the end of a [`FlatBlock`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FlatBlockEnd {
    /// This block returns to the call-site, outputting variables to the call-site.
    Callsite(Vec<VariableId>),
    /// This block ends with a `return` statement, exiting the function.
    Return(Vec<VariableId>),
    /// The last statement ended the flow (e.g., match will all arms ending in return),
    /// and the end of this block is unreachable.
    Unreachable,
}

impl TryFrom<StructuredBlock> for FlatBlock {
    type Error = ();

    fn try_from(value: StructuredBlock) -> Result<Self, Self::Error> {
        Ok(FlatBlock {
            inputs: value.inputs,
            statements: value.statements,
            drops: value.drops,
            end: value.end.try_into()?,
        })
    }
}

impl TryFrom<StructuredBlockEnd> for FlatBlockEnd {
    type Error = ();

    fn try_from(value: StructuredBlockEnd) -> Result<Self, Self::Error> {
        Ok(match value {
            StructuredBlockEnd::Callsite(vars) => FlatBlockEnd::Callsite(vars),
            StructuredBlockEnd::Return { refs, returns } => {
                FlatBlockEnd::Return(chain!(refs.iter(), returns.iter()).copied().collect())
            }
            StructuredBlockEnd::Panic { .. } => return Err(()),
            StructuredBlockEnd::Unreachable => FlatBlockEnd::Unreachable,
        })
    }
}

/// Lowered variable representation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Variable {
    /// Can the type be (trivially) dropped.
    pub droppable: bool,
    /// Can the type be (trivially) duplicated.
    pub duplicatable: bool,
    /// If this variable is a used as a reference variable (including implicits) of the current
    /// function, what are the indices of said reference variables?
    /// Note that a lowered variable might be assigned to multiple reference variables.
    pub ref_indices: OrderedHashSet<usize>,
    /// Semantic type of the variable.
    pub ty: semantic::TypeId,
}

/// Lowered statement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Statement {
    // Values.
    // TODO(spapini): Consts.
    Literal(StatementLiteral),

    // Flow control.
    Call(StatementCall),
    CallBlock(StatementCallBlock),
    MatchExtern(StatementMatchExtern),

    // Structs (including tuples).
    StructConstruct(StatementStructConstruct),
    StructDestructure(StatementStructDestructure),

    // Enums.
    EnumConstruct(StatementEnumConstruct),
    MatchEnum(StatementMatchEnum),
}
impl Statement {
    pub fn inputs(&self) -> Vec<VariableId> {
        match &self {
            Statement::Literal(_stmt) => vec![],
            Statement::Call(stmt) => stmt.inputs.clone(),
            Statement::CallBlock(_) => vec![],
            Statement::MatchExtern(stmt) => stmt.inputs.clone(),
            Statement::StructConstruct(stmt) => stmt.inputs.clone(),
            Statement::StructDestructure(stmt) => vec![stmt.input],
            Statement::EnumConstruct(stmt) => vec![stmt.input],
            Statement::MatchEnum(stmt) => vec![stmt.input],
        }
    }
    pub fn outputs(&self) -> Vec<VariableId> {
        match &self {
            Statement::Literal(stmt) => vec![stmt.output],
            Statement::Call(stmt) => stmt.outputs.clone(),
            Statement::CallBlock(stmt) => stmt.outputs.clone(),
            Statement::MatchExtern(stmt) => stmt.outputs.clone(),
            Statement::StructConstruct(stmt) => vec![stmt.output],
            Statement::StructDestructure(stmt) => stmt.outputs.clone(),
            Statement::EnumConstruct(stmt) => vec![stmt.output],
            Statement::MatchEnum(stmt) => stmt.outputs.clone(),
        }
    }
}

/// A statement that binds a literal value to a variable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatementLiteral {
    /// The type of the literal.
    pub ty: semantic::TypeId,
    /// The value of the literal.
    pub value: BigInt,
    /// The variable to bind the value to.
    pub output: VariableId,
}

/// A statement that calls a user function.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatementCall {
    /// A function to "call".
    pub function: semantic::FunctionId,
    /// Living variables in current scope to move to the function, as arguments.
    pub inputs: Vec<VariableId>,
    /// New variables to be introduced into the current scope from the function outputs.
    pub outputs: Vec<VariableId>,
}

/// A statement that jumps to another block. If that block ends with a BlockEnd::CallSite, the flow
/// returns to the statement following this one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatementCallBlock {
    /// A block to "call".
    pub block: BlockId,
    /// New variables to be introduced into the current scope, moved from the callee block outputs.
    pub outputs: Vec<VariableId>,
}

/// A statement that calls an extern function with branches, and "calls" a possibly different block
/// for each branch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatementMatchExtern {
    // TODO(spapini): ConcreteExternFunctionId once it exists.
    /// A concrete external function to call.
    pub function: semantic::FunctionId,
    /// Living variables in current scope to move to the function, as arguments.
    pub inputs: Vec<VariableId>,
    /// Match arms. All blocks should have the same rets.
    /// Order must be identical to the order in the definition of the enum.
    pub arms: Vec<(ConcreteVariant, BlockId)>,
    /// New variables to be introduced into the current scope from the arm outputs.
    pub outputs: Vec<VariableId>,
}

/// A statement that construct a variant of an enum with a single argument, and binds it to a
/// variable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatementEnumConstruct {
    pub variant: ConcreteVariant,
    /// A living variable in current scope to wrap with the variant.
    pub input: VariableId,
    /// The variable to bind the value to.
    pub output: VariableId,
}

/// A statement that matches an enum, and "calls" a possibly different block for each branch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatementMatchEnum {
    pub concrete_enum: ConcreteEnumId,
    /// A living variable in current scope to match on.
    pub input: VariableId,
    /// Match arms. All blocks should have the same rets.
    /// Order must be identical to the order in the definition of the enum.
    pub arms: Vec<(ConcreteVariant, BlockId)>,
    /// New variables to be introduced into the current scope from the arm outputs.
    pub outputs: Vec<VariableId>,
}

/// A statement that constructs a struct (tuple included) into a new variable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatementStructConstruct {
    pub inputs: Vec<VariableId>,
    /// The variable to bind the value to.
    pub output: VariableId,
}

/// A statement that destructures a struct (tuple included), introducing its elements as new
/// variables.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatementStructDestructure {
    /// A living variable in current scope to destructure.
    pub input: VariableId,
    /// The variables to bind values to.
    pub outputs: Vec<VariableId>,
}
