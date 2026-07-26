use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{fixtures::test_isolate, *};

const ARRAY_CONCAT_SOURCE: &str = r#"
var sparse = [1, , 3];
var result = sparse.concat([4, , 6], 7);
var sparseOk = result.length === 7 && result[0] === 1 && !(1 in result) &&
  result[2] === 3 && result[3] === 4 && !(4 in result) &&
  result[5] === 6 && result[6] === 7;

var arrayLike = { 0: 8, 2: 10, length: 3 };
arrayLike[Symbol.isConcatSpreadable] = true;
var generic = Array.prototype.concat.call(5, arrayLike);
var genericOk = generic.length === 4 && generic[0].valueOf() === 5 && generic[1] === 8 &&
  !(2 in generic) && generic[3] === 10;

var speciesCalls = 0;
function Result(length) { speciesCalls += length + 1; }
var source = [11];
source.constructor = { [Symbol.species]: Result };
var custom = source.concat(12);
var speciesOk = speciesCalls === 1 && custom instanceof Result && custom.length === 2 &&
  custom[0] === 11 && custom[1] === 12;

sparseOk && genericOk && speciesOk;
"#;

const ARRAY_CONCAT_PROXY_SOURCE: &str = r#"
var trace = "";
var source = { 0: 1, 2: 3, length: 3 };
source[Symbol.isConcatSpreadable] = true;
var proxy = new Proxy(source, {
  get: function(target, key, receiver) {
    trace += "g" + String(key) + ";";
    return Reflect.get(target, key, receiver);
  },
  has: function(target, key) {
    trace += "h" + key + ";";
    return key in target;
  }
});
var result = [].concat(proxy);
result.length === 3 && result[0] === 1 && !(1 in result) && result[2] === 3 &&
  trace === "gSymbol(Symbol.isConcatSpreadable);glength;h0;g0;h1;h2;g2;";
"#;

const ARRAY_CONCAT_STRING_SOURCE: &str = r#"
var value = new String("yuck");
value[Symbol.isConcatSpreadable] = true;
var result = [].concat(value);
result.length === 4 && result[0] === "y" && result[1] === "u" &&
  result[2] === "c" && result[3] === "k";
"#;

const ARRAY_CONCAT_LONG_SYNCHRONOUS_SOURCE: &str = r#"
var source = new Uint8Array(12000);
source[Symbol.isConcatSpreadable] = true;
var result = [].concat(source);
result.length === 12000 && result[0] === 0 && result[11999] === 0;
"#;

const ARRAY_STRING_ITERATOR_SOURCE: &str = r#"
[...'ab'][1] === 'b';
"#;

const ARRAY_ACCUMULATION_PROTOCOL_SOURCE: &str = r#"
var iteratorSymbol = Symbol.iterator;
var trace = "";
var source = {};
source[iteratorSymbol] = function() {
  trace += "i";
  var index = 0;
  return {
    next: function() {
      trace += "n" + index;
      if (index === 2) return { done: true };
      return { done: false, value: ++index };
    }
  };
};
Symbol = { iterator: "wrong" };
Array.prototype.concat = function() { throw new Error("observable concat"); };
var spread = [0, ...source, 3];

var sparse = [...[, 2]];
var elided = [...[], ,];
var astral = [...("A" + String.fromCodePoint(0x1f600) + "B")];
var unpaired = [...String.fromCharCode(0xd800)];
var numeric = String.prototype[iteratorSymbol].call(42);

var primitiveIteratorThrows = false;
var badIterator = {};
badIterator[iteratorSymbol] = function() { return 1; };
try { [...badIterator]; } catch (error) { primitiveIteratorThrows = error instanceof TypeError; }

var primitiveResultThrows = false;
var badResult = {};
badResult[iteratorSymbol] = function() { return { next: function() { return 1; } }; };
try { [...badResult]; } catch (error) { primitiveResultThrows = error instanceof TypeError; }

spread.length === 4 && spread[0] === 0 && spread[1] === 1 && spread[2] === 2 &&
  spread[3] === 3 && trace === "in0n1n2" &&
  sparse.length === 2 && (0 in sparse) && sparse[0] === undefined && sparse[1] === 2 &&
  elided.length === 1 && !(0 in elided) &&
  astral.length === 3 && astral[0] === "A" && astral[1].length === 2 && astral[2] === "B" &&
  unpaired.length === 1 && unpaired[0].length === 1 &&
  numeric.next().value === "4" && numeric.next().value === "2" && numeric.next().done === true &&
  primitiveIteratorThrows && primitiveResultThrows;
"#;

#[test]
fn array_concat_is_stable_for_every_dispatch_batch() {
    assert_array_concat_source::<1>(ARRAY_CONCAT_SOURCE, 1_801, false);
    assert_array_concat_source::<2>(ARRAY_CONCAT_SOURCE, 1_802, false);
    assert_array_concat_source::<4>(ARRAY_CONCAT_SOURCE, 1_804, false);
    assert_array_concat_source::<8>(ARRAY_CONCAT_SOURCE, 1_808, false);
    assert_array_concat_source::<16>(ARRAY_CONCAT_SOURCE, 1_816, false);
}

#[test]
fn array_concat_proxy_order_is_stable_for_every_dispatch_batch() {
    assert_array_concat_source::<1>(ARRAY_CONCAT_PROXY_SOURCE, 1_821, false);
    assert_array_concat_source::<2>(ARRAY_CONCAT_PROXY_SOURCE, 1_822, false);
    assert_array_concat_source::<4>(ARRAY_CONCAT_PROXY_SOURCE, 1_824, false);
    assert_array_concat_source::<8>(ARRAY_CONCAT_PROXY_SOURCE, 1_828, false);
    assert_array_concat_source::<16>(ARRAY_CONCAT_PROXY_SOURCE, 1_836, false);
}

#[test]
fn array_concat_string_exotic_indices_are_spreadable() {
    assert_array_concat_source::<8>(ARRAY_CONCAT_STRING_SOURCE, 1_840, false);
    assert_array_concat_source::<8>(ARRAY_STRING_ITERATOR_SOURCE, 1_843, false);
}

#[test]
/// Uses a larger atom quota because generic indexed copies materialize property keys.
fn array_concat_long_synchronous_copy_does_not_grow_the_rust_stack() {
    let module = compile_array_concat_source(ARRAY_CONCAT_LONG_SYNCHRONOUS_SOURCE, 1_844);
    let mut isolate = Isolate::new(IsolateConfig::new(
        AtomTableConfig::new(32_768, 4 * 1024 * 1024, AtomHashSeed::new(1, 2)),
        HeapLimit::new(32 * SPAN_SIZE_BYTES),
        StackLimits::new(64, 4_096),
        RealmLimits::new(64, 32_768).with_max_shapes(32_768),
    ))
    .expect("large-atom concat isolate initializes");
    let outcome = isolate
        .execute_with_batch::<8>(
            &module,
            ExecutionBudget {
                fuel: 32_768,
                quantum: 32_768,
            },
        )
        .expect("long synchronous concat executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "long synchronous concat returned {outcome:?}"
    );
}

#[test]
fn array_concat_state_survives_forced_major_collections() {
    assert_array_concat_source::<8>(ARRAY_CONCAT_SOURCE, 1_841, true);
    assert_array_concat_source::<8>(ARRAY_CONCAT_PROXY_SOURCE, 1_842, true);
}

#[test]
fn array_accumulation_obeys_iterator_protocol_for_every_dispatch_batch() {
    assert_array_concat_source::<1>(ARRAY_ACCUMULATION_PROTOCOL_SOURCE, 1_851, false);
    assert_array_concat_source::<2>(ARRAY_ACCUMULATION_PROTOCOL_SOURCE, 1_852, false);
    assert_array_concat_source::<4>(ARRAY_ACCUMULATION_PROTOCOL_SOURCE, 1_854, false);
    assert_array_concat_source::<8>(ARRAY_ACCUMULATION_PROTOCOL_SOURCE, 1_858, false);
    assert_array_concat_source::<16>(ARRAY_ACCUMULATION_PROTOCOL_SOURCE, 1_866, false);
}

#[test]
fn array_accumulation_iterator_state_survives_forced_major_collections() {
    assert_array_concat_source::<8>(ARRAY_ACCUMULATION_PROTOCOL_SOURCE, 1_868, true);
}

/// Compiles and executes one concat fixture under a selected dispatch and GC policy.
fn assert_array_concat_source<const N: usize>(source: &str, source_id: u32, forced_major: bool) {
    let module = compile_array_concat_source(source, source_id);
    let mut isolate = test_isolate();
    if forced_major {
        isolate
            .heap
            .set_forced_collection_mode(ForcedCollectionMode::Major);
    }
    let outcome = isolate
        .execute_with_batch::<N>(
            &module,
            ExecutionBudget {
                fuel: 32_768,
                quantum: 32_768,
            },
        )
        .expect("Array concat fixture executes");
    let completed_i32 = match outcome {
        RunOutcome::Completed(value) => value.as_i32(),
        _ => None,
    };
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N} returned {outcome:?}, i32={completed_i32:?}"
    );
}

/// Compiles one concat fixture independently of dispatch and heap policy.
fn compile_array_concat_source(source: &str, source_id: u32) -> CompiledModule {
    Compiler
        .compile(
            SourceText::new(
                SourceId::new(source_id),
                SourceName::new("array-concat-fixture"),
                MediaType::JavaScript,
                Arc::from(source),
            ),
            CompileOptions::default(),
        )
        .expect("Array concat fixture compiles")
}
