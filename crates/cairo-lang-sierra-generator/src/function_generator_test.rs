use cairo_lang_defs::ids::ModuleItemId;
use cairo_lang_lowering::db::LoweringGroup;
use cairo_lang_semantic::db::SemanticGroup;
use cairo_lang_semantic::test_utils::setup_test_module;
use cairo_lang_utils::extract_matches;
use indoc::indoc;
use pretty_assertions::assert_eq;
use test_log::test;

use crate::db::SierraGenGroup;
use crate::replace_ids::replace_sierra_ids;
use crate::test_utils::SierraGenDatabaseForTesting;

#[test]
fn test_function_generator() {
    let mut db = SierraGenDatabaseForTesting::default();
    let module_id = setup_test_module(
        &mut db,
        indoc! {"
                fn foo(a: felt, b: felt) -> felt {
                    let b = felt_add(a, 5);
                    bar(b, b, b)
                }

                fn bar(x: felt, y: felt, z: felt) -> felt {
                    0
                }
            "},
    )
    .unwrap()
    .module_id;
    let foo = extract_matches!(
        db.module_item_by_name(module_id, "foo".into()).unwrap().unwrap(),
        ModuleItemId::FreeFunction,
        "Unexpected item type."
    );

    db.module_lowering_diagnostics(module_id).expect("");
    let function = db.free_function_sierra(foo).unwrap();
    assert_eq!(
        function
            .body
            .iter()
            .map(|x| replace_sierra_ids(&db, x).to_string())
            .collect::<Vec<String>>(),
        vec![
            "label1:",
            "drop<felt>([1]) -> ()",
            "felt_const<5>() -> ([2])",
            "felt_add([0], [2]) -> ([3])",
            "store_temp<felt>([3]) -> ([3])",
            "dup<felt>([3]) -> ([3], [9])",
            "rename<felt>([9]) -> ([5])",
            "dup<felt>([3]) -> ([3], [10])",
            "store_temp<felt>([10]) -> ([6])",
            "store_temp<felt>([3]) -> ([7])",
            "function_call<user@test::bar>([5], [6], [7]) -> ([4])",
            "rename<felt>([4]) -> ([8])",
            "return([8])",
        ]
    );
    assert_eq!(function.entry_point.to_string(), "label1");
}

#[test]
fn test_function_generator_local_vars() {
    let mut db = SierraGenDatabaseForTesting::default();
    let module_id = setup_test_module(
        &mut db,
        indoc! {"
            fn foo(a: felt) -> felt {
                let b = a + a + a;
                revoke_ap();
                b
            }

            // Revokes ap since this function is recursive.
            fn revoke_ap() -> felt {
                revoke_ap()
            }
        "},
    )
    .unwrap()
    .module_id;
    let foo = extract_matches!(
        db.module_item_by_name(module_id, "foo".into()).unwrap().unwrap(),
        ModuleItemId::FreeFunction,
        "Unexpected item type."
    );

    db.module_lowering_diagnostics(module_id).expect("");
    let function = db.free_function_sierra(foo).unwrap();
    assert_eq!(
        function
            .body
            .iter()
            .map(|x| replace_sierra_ids(&db, x).to_string())
            .collect::<Vec<String>>(),
        vec![
            "label0:",
            "alloc_local<felt>() -> ([2])",
            "finalize_locals() -> ()",
            "dup<felt>([0]) -> ([0], [6])",
            "dup<felt>([0]) -> ([0], [7])",
            "felt_add([6], [7]) -> ([3])",
            "store_temp<felt>([3]) -> ([3])",
            "felt_add([3], [0]) -> ([1])",
            "store_local<felt>([2], [1]) -> ([1])",
            "function_call<user@test::revoke_ap>() -> ([4])",
            "drop<felt>([4]) -> ()",
            "store_temp<felt>([1]) -> ([5])",
            "return([5])",
        ]
    );
    assert_eq!(function.entry_point.to_string(), "label0");
}
