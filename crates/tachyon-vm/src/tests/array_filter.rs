use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{fixtures::test_isolate, *};

const FILTER_PROXY_SOURCE: &str = r#"
var target = {};
var trace = "";
var resultProxy = new Proxy(target, {
  defineProperty: function(object, key, descriptor) {
    trace += key + descriptor.value;
    if (!descriptor.writable || !descriptor.enumerable || !descriptor.configurable) throw 91;
    Object.defineProperty(object, key, descriptor);
    return true;
  }
});
var source = [4, 7];
source.constructor = {};
source.constructor[Symbol.species] = function() { return resultProxy; };
var result = source.filter(function() { return true; });
result === resultProxy && trace === "0417" && result[0] === 4 && result[1] === 7;
"#;

const FILTER_ORDINARY_SOURCE: &str = r#"
var source = [4, 7];
var result = source.filter(function() { return true; });
result.length === 2 && result[0] === 4 && result[1] === 7;
"#;

const ARRAY_LITERAL_INHERITED_GETTER_SOURCE: &str = r#"
Object.defineProperty(Array.prototype, "0", {
  get: function() { return 9; },
  configurable: true
});
var result = [11].filter(function(value) { return value === 11; });
result.length === 1 && result[0] === 11;
"#;

const FILTER_CROSS_REALM_ARRAY_CONSTRUCTOR_SOURCE: &str = r#"
var array = [];
var callCount = 0;
var speciesDesc = {
  get: function() { callCount += 1; }
};
array.constructor = foreignArray;
Object.defineProperty(Array, Symbol.species, speciesDesc);
Object.defineProperty(foreignArray, Symbol.species, speciesDesc);
var result = array.filter(function() {});
Object.getPrototypeOf(result) === Array.prototype && callCount === 0;
"#;

#[test]
fn array_filter_proxy_define_is_stable_for_every_dispatch_batch() {
    assert_filter_proxy_batch::<1>();
    assert_filter_proxy_batch::<2>();
    assert_filter_proxy_batch::<4>();
    assert_filter_proxy_batch::<8>();
    assert_filter_proxy_batch::<16>();
}

#[test]
fn array_filter_species_state_survives_forced_major_collections() {
    let module = compile_filter_source(FILTER_ORDINARY_SOURCE, 1_305);
    let mut isolate = test_isolate();
    isolate
        .heap
        .set_forced_collection_mode(ForcedCollectionMode::Major);
    let outcome = isolate
        .execute_with_batch::<8>(
            &module,
            ExecutionBudget {
                fuel: 4_096,
                quantum: 4_096,
            },
        )
        .expect("forced-major filter Proxy fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "forced-major filter Proxy fixture returned {outcome:?}"
    );
}

#[test]
fn array_filter_proxy_define_survives_forced_major_collections() {
    let module = compile_filter_source(FILTER_PROXY_SOURCE, 1_306);
    let mut isolate = test_isolate();
    isolate
        .heap
        .set_forced_collection_mode(ForcedCollectionMode::Major);
    let outcome = isolate
        .execute_with_batch::<8>(
            &module,
            ExecutionBudget {
                fuel: 4_096,
                quantum: 4_096,
            },
        )
        .expect("forced-major filter Proxy fixture executes");
    assert!(matches!(
        outcome,
        RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)
    ));
}

#[test]
fn array_literal_elements_ignore_inherited_accessors_for_every_dispatch_batch() {
    assert_array_literal_inherited_getter_batch::<1>();
    assert_array_literal_inherited_getter_batch::<2>();
    assert_array_literal_inherited_getter_batch::<4>();
    assert_array_literal_inherited_getter_batch::<8>();
    assert_array_literal_inherited_getter_batch::<16>();
}

#[test]
fn array_filter_ignores_another_realms_intrinsic_array_constructor() {
    assert_cross_realm_array_constructor_batch::<1>();
    assert_cross_realm_array_constructor_batch::<2>();
    assert_cross_realm_array_constructor_batch::<4>();
    assert_cross_realm_array_constructor_batch::<8>();
    assert_cross_realm_array_constructor_batch::<16>();
}

/// Publishes a child Realm's intrinsic Array constructor and executes one filter dispatch batch.
fn assert_cross_realm_array_constructor_batch<const N: usize>() {
    let module = compile_filter_source(
        FILTER_CROSS_REALM_ARRAY_CONSTRUCTOR_SOURCE,
        1_330 + N as u32,
    );
    let mut isolate = test_isolate();
    let (_, child_global) = isolate.create_realm().unwrap();
    let array_atom = isolate.intern_intrinsic_name(b"Array").unwrap();
    let foreign_array = isolate
        .get_data_property(child_global, array_atom)
        .unwrap()
        .expect("child Realm publishes Array");
    let foreign_array_atom = isolate.intern_intrinsic_name(b"foreignArray").unwrap();
    let global = isolate.realm.global_object.unwrap();
    isolate
        .set_own_data_property(global, foreign_array_atom, foreign_array)
        .unwrap();
    isolate
        .realm
        .set(foreign_array_atom, foreign_array)
        .unwrap();
    let outcome = isolate
        .execute_with_batch::<N>(
            &module,
            ExecutionBudget {
                fuel: 4_096,
                quantum: 4_096,
            },
        )
        .expect("cross-Realm ArraySpeciesCreate fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N} returned {outcome:?}"
    );
}

/// Executes the inherited-accessor regression through one interpreter dispatch batch size.
fn assert_array_literal_inherited_getter_batch<const N: usize>() {
    let module = compile_filter_source(ARRAY_LITERAL_INHERITED_GETTER_SOURCE, 1_310 + N as u32);
    let mut isolate = test_isolate();
    let outcome = isolate
        .execute_with_batch::<N>(
            &module,
            ExecutionBudget {
                fuel: 4_096,
                quantum: 4_096,
            },
        )
        .expect("array literal inherited-accessor fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N} returned {outcome:?}"
    );
}

/// Compiles and executes the resumable Proxy result fixture for one dispatch batch.
fn assert_filter_proxy_batch<const N: usize>() {
    let module = compile_filter_proxy_source(1_300 + N as u32);
    let mut isolate = test_isolate();
    let outcome = isolate
        .execute_with_batch::<N>(
            &module,
            ExecutionBudget {
                fuel: 4_096,
                quantum: 4_096,
            },
        )
        .expect("filter Proxy fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N} returned {outcome:?}"
    );
}

fn compile_filter_proxy_source(source_id: u32) -> CompiledModule {
    compile_filter_source(FILTER_PROXY_SOURCE, source_id)
}

fn compile_filter_source(source: &str, source_id: u32) -> CompiledModule {
    Compiler
        .compile(
            SourceText::new(
                SourceId::new(source_id),
                SourceName::new("array-filter-proxy-define"),
                MediaType::JavaScript,
                Arc::from(source),
            ),
            CompileOptions::default(),
        )
        .expect("filter Proxy fixture compiles")
}
