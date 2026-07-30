use super::{fixtures::test_isolate, *};
use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

const OPTIONAL_CHAIN_SOURCE: &str = r#"
var effects = 0;
var absent = null;
var shortMember = absent?.[++effects].stillSkipped;
var shortCall = absent?.(effects += 10);
var receiver = {
  marker: 42,
  method() { return this.marker; }
};
var receiverOk = receiver?.method() === 42 &&
  (receiver?.method)() === 42 &&
  receiver.method?.() === 42 &&
  (receiver.method)?.() === 42 &&
  receiver?.method?.() === 42 &&
  (receiver?.method)?.() === 42 &&
  ((receiver?.method))?.() === 42;
var boundaryThrows = false;
try { (absent?.property).nested; } catch (error) {
  boundaryThrows = error instanceof TypeError;
}
var nonCallableThrowsAfterArguments = false;
try { ({ value: 1 }).value?.(effects += 100); } catch (error) {
  nonCallableThrowsAfterArguments = error instanceof TypeError && effects === 100;
}
var target = { removable: 1 };
var deleteOk = delete absent?.removable;
var deletePresent = delete target?.removable;
class Base {}
class Derived extends Base {
  constructor() {
    var missing = super()?.missing;
    this.superChainOk = missing === undefined;
  }
}
var superChainOk = new Derived().superChainOk;
effects === 100 && shortMember === undefined && shortCall === undefined &&
  receiverOk && boundaryThrows && nonCallableThrowsAfterArguments &&
  deleteOk === true && deletePresent === true && !("removable" in target) &&
  superChainOk;
"#;

const OPTIONAL_EVAL_SOURCE: &str = r#"
var optionalEvalGlobal = "global";
function readOptionalEval() {
  var optionalEvalGlobal = "local";
  return eval?.("optionalEvalGlobal");
}
readOptionalEval() === "global";
"#;

#[test]
fn optional_chain_semantics_are_stable_for_every_dispatch_batch() {
    assert_optional_source::<1>(OPTIONAL_CHAIN_SOURCE, 2_601, false);
    assert_optional_source::<2>(OPTIONAL_CHAIN_SOURCE, 2_602, false);
    assert_optional_source::<4>(OPTIONAL_CHAIN_SOURCE, 2_604, false);
    assert_optional_source::<8>(OPTIONAL_CHAIN_SOURCE, 2_608, false);
    assert_optional_source::<16>(OPTIONAL_CHAIN_SOURCE, 2_616, false);
}

#[test]
fn optional_call_and_indirect_eval_survive_forced_major() {
    assert_optional_source::<8>(
        "var effects = 0; var value = null?.[++effects].missing; effects === 0 && value === undefined;",
        2_620,
        true,
    );
    assert_optional_source::<8>(
        "var object = { marker: 42, method() { return this.marker; } }; object?.method?.() === 42 && (object?.method)?.() === 42;",
        2_621,
        true,
    );
    assert_optional_source::<8>(
        "var object = { value: 1 }; var removed = delete object?.value; removed && !(\"value\" in object);",
        2_622,
        true,
    );
    assert_optional_source::<8>(
        "class Base {} class Derived extends Base { constructor() { var value = super()?.missing; this.ok = value === undefined; } } new Derived().ok;",
        2_623,
        true,
    );
    assert_optional_source::<8>(OPTIONAL_EVAL_SOURCE, 2_624, true);
}

/// Compiles and executes one optional-chain fixture under the selected dispatch and GC policy.
fn assert_optional_source<const N: usize>(source: &str, source_id: u32, forced_major: bool) {
    let module = Compiler
        .compile(
            SourceText::new(
                SourceId::new(source_id),
                SourceName::new("optional-chaining-fixture"),
                MediaType::JavaScript,
                Arc::from(source),
            ),
            CompileOptions::default(),
        )
        .expect("optional-chaining fixture compiles");
    let mut isolate = test_isolate();
    isolate
        .install_realm_hooks(
            super::eval::eval_script_callback,
            super::eval::dynamic_function_callback,
        )
        .expect("optional-chain eval hooks install");
    if forced_major {
        isolate
            .heap
            .set_forced_collection_mode(ForcedCollectionMode::Major);
    }
    let outcome = isolate
        .execute_with_batch::<N>(
            &module,
            ExecutionBudget {
                fuel: 65_536,
                quantum: 65_536,
            },
        )
        .expect("optional-chaining fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "source {source_id} with dispatch batch {N} returned {outcome:?}"
    );
}
