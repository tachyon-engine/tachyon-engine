use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{fixtures::test_isolate, *};

const MAP_SEMANTICS_SOURCE: &str = r#"
var context = { bias: 10 };
var source = [2, , 6];
var calls = "";
var result = source.map(function(value, index, receiver) {
  calls += value + ":" + index + ":" + (receiver === source) + ";";
  source.push(99);
  return value + this.bias;
}, context);
result.length === 3 && result[0] === 12 && !(1 in result) && result[2] === 16 &&
calls === "2:0:true;6:2:true;";
"#;

const MAP_INHERITED_SOURCE: &str = r#"
Object.defineProperty(Array.prototype, "1", {
  value: 7,
  writable: true,
  configurable: true
});
var source = [3, , 5];
var result = source.map(function(value) { return value * 2; });
result.length === 3 && result[0] === 6 && result[1] === 14 && result[2] === 10;
"#;

const MAP_PROXY_SPECIES_SOURCE: &str = r#"
var sourceTrace = "";
var target = [4, , 8];
var source = new Proxy(target, {
  get: function(object, key) {
    sourceTrace += "g" + key + ";";
    return object[key];
  },
  has: function(object, key) {
    sourceTrace += "h" + key + ";";
    return key in object;
  }
});
var resultTarget = {};
var defineTrace = "";
var resultProxy = new Proxy(resultTarget, {
  defineProperty: function(object, key, descriptor) {
    defineTrace += key + ":" + descriptor.value + ";";
    if (!descriptor.writable || !descriptor.enumerable || !descriptor.configurable) throw 91;
    Object.defineProperty(object, key, descriptor);
    return true;
  }
});
var constructorLength = -1;
source.constructor = {};
source.constructor[Symbol.species] = function(length) {
  constructorLength = length;
  return resultProxy;
};
sourceTrace = "";
var result = Array.prototype.map.call(source, function(value) { return value + 1; });
result === resultProxy && constructorLength === 3 &&
sourceTrace === "glength;gconstructor;h0;g0;h1;h2;g2;" &&
defineTrace === "0:5;2:9;" && resultTarget[0] === 5 && resultTarget[2] === 9;
"#;

const MAP_ABRUPT_SOURCE: &str = r#"
var marker = { marker: true };
var callbackIdentity = false;
try {
  [1].map(function() { throw marker; });
} catch (error) {
  callbackIdentity = error === marker;
}
var defineRejected = false;
var source = [1];
source.constructor = {};
source.constructor[Symbol.species] = function() {
  return new Proxy({}, { defineProperty: function() { return false; } });
};
try {
  source.map(function(value) { return value; });
} catch (error) {
  defineRejected = true;
}
var invalidLengthIsRangeError = false;
try {
  Array.prototype.map.call({ length: 4294967296 }, function() {});
} catch (error) {
  invalidLengthIsRangeError = error instanceof RangeError;
}
callbackIdentity && defineRejected && invalidLengthIsRangeError;
"#;

const MAP_FORCED_MAJOR_SOURCE: &str = r#"
var resultTarget = {};
var resultProxy = new Proxy(resultTarget, {
  defineProperty: function(object, key, descriptor) {
    Object.defineProperty(object, key, descriptor);
    return true;
  }
});
var observedLength = -1;
var source = [4, 7];
source.constructor = {};
source.constructor[Symbol.species] = function(length) {
  observedLength = length;
  return resultProxy;
};
var result = source.map(function(value) { return value + 1; });
result === resultProxy && observedLength === 2 && resultTarget[0] === 5 && resultTarget[1] === 8;
"#;

const MAP_SPARSE_SOURCE: &str = r#"
var sparse = [];
sparse[999999] = 7;
var callbackCount = 0;
var mapped = sparse.map(function(value) {
  callbackCount += 1;
  return value + 1;
});
var hasCount = 0;
var proxy = new Proxy({ 299: 4, length: 300 }, {
  has: function(target, key) {
    hasCount += 1;
    return key in target;
  }
});
var proxyResult = Array.prototype.map.call(proxy, function(value) { return value + 1; });
callbackCount === 1 && mapped.length === 1000000 && mapped[999999] === 8 &&
hasCount === 300 && proxyResult.length === 300 && proxyResult[299] === 5;
"#;

#[test]
fn array_map_semantics_are_stable_for_every_dispatch_batch() {
    assert_map_batch::<1>(MAP_SEMANTICS_SOURCE, 1_400);
    assert_map_batch::<2>(MAP_SEMANTICS_SOURCE, 1_410);
    assert_map_batch::<4>(MAP_SEMANTICS_SOURCE, 1_420);
    assert_map_batch::<8>(MAP_SEMANTICS_SOURCE, 1_430);
    assert_map_batch::<16>(MAP_SEMANTICS_SOURCE, 1_440);
}

#[test]
fn array_map_observes_inherited_indexed_properties() {
    assert_map_batch::<8>(MAP_INHERITED_SOURCE, 1_450);
}

#[test]
fn array_map_proxy_and_species_order_is_resumable() {
    assert_map_batch::<1>(MAP_PROXY_SPECIES_SOURCE, 1_460);
    assert_map_batch::<2>(MAP_PROXY_SPECIES_SOURCE, 1_470);
    assert_map_batch::<4>(MAP_PROXY_SPECIES_SOURCE, 1_480);
    assert_map_batch::<8>(MAP_PROXY_SPECIES_SOURCE, 1_490);
    assert_map_batch::<16>(MAP_PROXY_SPECIES_SOURCE, 1_500);
}

#[test]
fn array_map_preserves_abrupt_identity_and_rejects_false_define() {
    assert_map_batch::<8>(MAP_ABRUPT_SOURCE, 1_510);
}

#[test]
fn array_map_state_survives_forced_major_collections() {
    let module = compile_map_source(MAP_FORCED_MAJOR_SOURCE, 1_520);
    let mut isolate = test_isolate();
    isolate
        .heap
        .set_forced_collection_mode(ForcedCollectionMode::Major);
    let outcome = isolate
        .execute_with_batch::<8>(
            &module,
            ExecutionBudget {
                fuel: 8_192,
                quantum: 8_192,
            },
        )
        .expect("forced-major map fixture executes");
    assert!(matches!(
        outcome,
        RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)
    ));
}

#[test]
fn array_map_skips_only_proven_ordinary_holes() {
    assert_map_batch::<8>(MAP_SPARSE_SOURCE, 1_530);
}

/// Compiles and executes one map fixture through a selected interpreter batch size.
fn assert_map_batch<const N: usize>(source: &str, source_id: u32) {
    let module = compile_map_source(source, source_id + N as u32);
    let outcome = test_isolate()
        .execute_with_batch::<N>(
            &module,
            ExecutionBudget {
                fuel: 8_192,
                quantum: 8_192,
            },
        )
        .expect("map fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N} returned {outcome:?}"
    );
}

fn compile_map_source(source: &str, source_id: u32) -> CompiledModule {
    Compiler
        .compile(
            SourceText::new(
                SourceId::new(source_id),
                SourceName::new("array-map"),
                MediaType::JavaScript,
                Arc::from(source),
            ),
            CompileOptions::default(),
        )
        .expect("map fixture compiles")
}
