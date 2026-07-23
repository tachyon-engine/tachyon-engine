use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{fixtures::test_isolate, *};

const ARRAY_SPLICE_SIMPLE_SOURCE: &str = r#"
var source = [0, 1, 2];
var removed = source.splice(1, 1);
removed.length === 1 && removed[0] === 1 && source.length === 2 &&
  source[0] === 0 && source[1] === 2;
"#;

const ARRAY_SPLICE_SOURCE: &str = r#"
var grow = [0, 1, 2, 3];
var removed = grow.splice(1, 2, 7, 8, 9);
var growOk = removed.length === 2 && removed[0] === 1 && removed[1] === 2 &&
  grow.length === 5 && grow[0] === 0 && grow[1] === 7 && grow[2] === 8 &&
  grow[3] === 9 && grow[4] === 3;

var shrink = [0, 1, 2, 3, 4];
var shrunk = shrink.splice(-4, 3, 6);
var shrinkOk = shrunk.length === 3 && shrunk[0] === 1 && shrunk[1] === 2 &&
  shrunk[2] === 3 && shrink.length === 3 && shrink[0] === 0 &&
  shrink[1] === 6 && shrink[2] === 4 && !(3 in shrink);

var sparse = [, 1, , 3];
var holes = sparse.splice(0, 3);
var holesOk = holes.length === 3 && !(0 in holes) && holes[1] === 1 &&
  !(2 in holes) && sparse.length === 1 && sparse[0] === 3;

var none = [1, 2];
var noneResult = none.splice();
var omitted = [1, 2];
var omittedResult = omitted.splice(1);
var explicit = [1, 2];
var explicitResult = explicit.splice(1, undefined);
var argumentsOk = none.length === 2 && noneResult.length === 0 &&
  omitted.length === 1 && omittedResult.length === 1 && omittedResult[0] === 2 &&
  explicit.length === 2 && explicitResult.length === 0;

var trace = "";
var generic = {
  0: 4,
  1: 5,
  2: 6,
  _length: 3,
  get length() { trace += "l"; return this._length; },
  set length(value) { this._length = value; }
};
var start = { valueOf: function() { trace += "s"; return 1; } };
var count = { valueOf: function() { trace += "d"; return 1; } };
var genericResult = Array.prototype.splice.call(generic, start, count, 8);
var genericOk = trace === "lsd" && genericResult.length === 1 && genericResult[0] === 5 &&
  generic._length === 3 && generic[0] === 4 && generic[1] === 8 && generic[2] === 6;

growOk && shrinkOk && holesOk && argumentsOk && genericOk;
"#;

const ARRAY_SPLICE_PROXY_SOURCE: &str = r#"
var trace = "";
var target = { 0: 1, 2: 3, length: 3 };
var proxy = new Proxy(target, {
  get: function(object, key, receiver) {
    trace += "g" + key + ";";
    return Reflect.get(object, key, receiver);
  },
  has: function(object, key) {
    trace += "h" + key + ";";
    return key in object;
  },
  set: function(object, key, value, receiver) {
    trace += "s" + key + ";";
    object[key] = value;
    return true;
  },
  deleteProperty: function(object, key) {
    trace += "d" + key + ";";
    delete object[key];
    return true;
  }
});
var removed = Array.prototype.splice.call(proxy, 0, 2, 7);
removed.length === 2 && removed[0] === 1 && !(1 in removed) &&
  target.length === 2 && target[0] === 7 && target[1] === 3 &&
  trace === "glength;h0;g0;h1;h2;g2;s1;d2;s0;slength;";
"#;

#[test]
fn array_splice_is_stable_for_every_dispatch_batch() {
    assert_array_splice_source::<1>(ARRAY_SPLICE_SOURCE, 1_701, false);
    assert_array_splice_source::<2>(ARRAY_SPLICE_SOURCE, 1_702, false);
    assert_array_splice_source::<4>(ARRAY_SPLICE_SOURCE, 1_704, false);
    assert_array_splice_source::<8>(ARRAY_SPLICE_SOURCE, 1_708, false);
    assert_array_splice_source::<16>(ARRAY_SPLICE_SOURCE, 1_716, false);
}

#[test]
fn array_splice_simple_path_executes() {
    assert_array_splice_source::<1>(ARRAY_SPLICE_SIMPLE_SOURCE, 1_700, false);
}

#[test]
fn array_splice_proxy_order_is_stable_for_every_dispatch_batch() {
    assert_array_splice_source::<1>(ARRAY_SPLICE_PROXY_SOURCE, 1_721, false);
    assert_array_splice_source::<2>(ARRAY_SPLICE_PROXY_SOURCE, 1_722, false);
    assert_array_splice_source::<4>(ARRAY_SPLICE_PROXY_SOURCE, 1_724, false);
    assert_array_splice_source::<8>(ARRAY_SPLICE_PROXY_SOURCE, 1_728, false);
    assert_array_splice_source::<16>(ARRAY_SPLICE_PROXY_SOURCE, 1_736, false);
}

#[test]
fn array_splice_state_survives_forced_major_collections() {
    assert_array_splice_source::<8>(ARRAY_SPLICE_SIMPLE_SOURCE, 1_739, true);
    assert_array_splice_source::<8>(ARRAY_SPLICE_SOURCE, 1_740, true);
    assert_array_splice_source::<8>(ARRAY_SPLICE_PROXY_SOURCE, 1_741, true);
}

/// Compiles and executes one splice fixture under a selected dispatch and GC policy.
fn assert_array_splice_source<const N: usize>(source: &str, source_id: u32, forced_major: bool) {
    let module = compile_array_splice_source(source, source_id);
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
        .expect("Array splice fixture executes");
    let thrown = match outcome {
        RunOutcome::Thrown(error) => {
            let kind = isolate.native_error_kind(error).unwrap();
            let message = isolate
                .intern_intrinsic_name(b"message")
                .ok()
                .and_then(|key| isolate.get_data_property(error, key).ok().flatten())
                .and_then(|value| isolate.primitive_string_units(value).ok())
                .and_then(|units| String::from_utf16(&units).ok());
            Some((kind, message))
        }
        _ => None,
    };
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N} returned {outcome:?}, thrown={thrown:?}"
    );
}

/// Compiles one splice fixture without coupling it to an isolate collection policy.
fn compile_array_splice_source(source: &str, source_id: u32) -> CompiledModule {
    Compiler
        .compile(
            SourceText::new(
                SourceId::new(source_id),
                SourceName::new("array-splice-fixture"),
                MediaType::JavaScript,
                Arc::from(source),
            ),
            CompileOptions::default(),
        )
        .expect("Array splice fixture compiles")
}
