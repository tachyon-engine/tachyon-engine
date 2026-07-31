use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{fixtures::test_isolate, *};

const GET_SET_RECORD_SOURCE: &str = r#"
var trace = "";
var done = false;
var other = {
  get size() {
    trace += "s";
    return { valueOf() { trace += "n"; return 1; } };
  },
  get has() {
    trace += "h";
    return function() { return false; };
  },
  get keys() {
    trace += "k";
    return function() {
      trace += "K";
      return {
        next() {
          trace += "i";
          if (done) return { value: undefined, done: true };
          done = true;
          return { value: 2, done: false };
        }
      };
    };
  }
};
var result = new Set([1]).union(other);
trace === "snhkKii" && Array.from(result).join(",") === "1,2";
"#;

const LIVE_MUTATION_SOURCE: &str = r#"
var intersectionReceiver = new Set([1]);
var intersectionCalls = "";
var intersectionOther = {
  size: 10,
  has(value) {
    intersectionCalls += value;
    if (value === 1) {
      intersectionReceiver.add(2);
      return false;
    }
    return true;
  },
  keys() { throw new Error("keys must not be called"); }
};
var intersectionResult = intersectionReceiver.intersection(intersectionOther);

var differenceReceiver = new Set([1, 9]);
var differenceDone = false;
var differenceOther = {
  size: 1,
  has() { throw new Error("has must not be called"); },
  keys() {
    differenceReceiver.add(7);
    return {
      next() {
        if (differenceDone) return { done: true };
        differenceDone = true;
        return { value: 9, done: false };
      }
    };
  },
};
var differenceResult = differenceReceiver.difference(differenceOther);

var subsetReceiver = new Set([1]);
var subsetCalls = "";
var subsetOther = {
  size: 10,
  has(value) {
    subsetCalls += value;
    if (value === 1) subsetReceiver.add(2);
    return true;
  },
  keys() { throw new Error("keys must not be called"); }
};
var subsetResult = subsetReceiver.isSubsetOf(subsetOther);

var unionReceiver = new Set([1]);
var unionDone = false;
var unionOther = {
  size: 1,
  has() { return false; },
  keys() {
    unionReceiver.add(9);
    return {
      next() {
        if (unionDone) return { done: true };
        unionDone = true;
        return { value: 2, done: false };
      }
    };
  }
};
var unionResult = unionReceiver.union(unionOther);

var symmetricReceiver = new Set([1]);
var symmetricIndex = 0;
var symmetricOther = {
  size: 2,
  has() { return false; },
  keys() {
    symmetricReceiver.add(9);
    return {
      next() {
        symmetricIndex++;
        if (symmetricIndex === 1) return { value: 2, done: false };
        if (symmetricIndex === 2) return { value: 3, done: false };
        return { done: true };
      }
    };
  }
};
var symmetricResult = symmetricReceiver.symmetricDifference(symmetricOther);

Array.from(intersectionResult).join(",") === "2" && intersectionCalls === "12" &&
Array.from(differenceReceiver).join(",") === "1,9,7" &&
Array.from(differenceResult).join(",") === "1" &&
subsetResult === true && subsetCalls === "12" &&
Array.from(unionReceiver).join(",") === "1,9" &&
Array.from(unionResult).join(",") === "1,9,2" &&
Array.from(symmetricReceiver).join(",") === "1,9" &&
Array.from(symmetricResult).join(",") === "1,9,2,3";
"#;

const ITERATOR_CLOSE_SOURCE: &str = r#"
function closingSetLike(value, trace) {
  var done = false;
  return {
    size: 1,
    has() { return false; },
    keys() {
      trace.value += "k";
      return {
        next() {
          trace.value += "n";
          if (done) return { done: true };
          done = true;
          return { value: value, done: false };
        },
        get return() {
          trace.value += "g";
          return function() {
            trace.value += "r";
            return {};
          };
        }
      };
    }
  };
}
var supersetTrace = { value: "" };
var superset = new Set([1, 2]);
var supersetResult = superset.isSupersetOf(closingSetLike(9, supersetTrace));
var disjointTrace = { value: "" };
var disjoint = new Set([1, 2]);
var disjointResult = disjoint.isDisjointFrom(closingSetLike(1, disjointTrace));
supersetResult === false && disjointResult === false &&
supersetTrace.value === "kngr" && disjointTrace.value === "kngr";
"#;

const RESULT_CONSTRUCTION_SOURCE: &str = r#"
class SubSet extends Set {}
var unionReceiver = new SubSet([3, 1]);
var unionOther = new Set([1, 2]);
var differenceReceiver = new Set([3, 1, 2]);
var differenceOther = new Set([1]);
var intersectionReceiver = new Set([3, 1, 2]);
var intersectionOther = new Set([1, 3]);
var symmetricReceiver = new Set([3, 1]);
var symmetricOther = new Set([1, 2]);
var emptyOther = new Set([4]);
var speciesCalls = 0;
Object.defineProperty(SubSet, Symbol.species, {
  get() { speciesCalls++; throw new Error("species must not be read"); }
});
var addCalls = 0;
var originalAdd = Set.prototype.add;
Set.prototype.add = function() {
  addCalls++;
  throw new Error("add must not be called");
};
var unionResult = unionReceiver.union(unionOther);
var differenceResult = differenceReceiver.difference(differenceOther);
var intersectionResult = intersectionReceiver.intersection(intersectionOther);
var symmetricResult = symmetricReceiver.symmetricDifference(symmetricOther);
var emptyUnionResult = new Set().union(emptyOther);
Set.prototype.add = originalAdd;

speciesCalls === 0 && addCalls === 0 &&
Object.getPrototypeOf(unionResult) === Set.prototype && !(unionResult instanceof SubSet) &&
Array.from(unionResult).join(",") === "3,1,2" &&
Array.from(differenceResult).join(",") === "3,2" &&
Array.from(intersectionResult).join(",") === "1,3" &&
Array.from(symmetricResult).join(",") === "3,2" &&
Array.from(emptyUnionResult).join(",") === "4";
"#;

const GENERATOR_SET_LIKE_SOURCE: &str = r#"
var receiver = new Set(["a", "b", "c"]);
var other = {
  size: 3,
  has() { throw new Error("has must not be called"); },
  *keys() {
    yield "a";
    receiver.delete("b");
    receiver.delete("c");
    receiver.add("b");
    yield "b";
  }
};
var superset = receiver.isSupersetOf(other);

var union = new Set([1, 2]).union({
  size: 2,
  has() { throw new Error("has must not be called"); },
  keys: function* keys() {
    yield 2;
    yield 3;
  }
});

superset === true && Array.from(receiver).join(",") === "a,b" &&
Array.from(union).join(",") === "1,2,3";
"#;

const SET_METHOD_FIXTURES: [(&str, &str); 5] = [
    ("GetSetRecord", GET_SET_RECORD_SOURCE),
    ("live mutation", LIVE_MUTATION_SOURCE),
    ("IteratorClose", ITERATOR_CLOSE_SOURCE),
    ("result construction", RESULT_CONSTRUCTION_SOURCE),
    ("generator set-like", GENERATOR_SET_LIKE_SOURCE),
];

#[test]
fn set_methods_are_stable_for_every_dispatch_batch() {
    assert_set_methods_batch::<1>(false);
    assert_set_methods_batch::<2>(false);
    assert_set_methods_batch::<4>(false);
    assert_set_methods_batch::<8>(false);
    assert_set_methods_batch::<16>(false);
}

#[test]
fn set_methods_survive_forced_major_collection() {
    assert_set_methods_batch::<8>(true);
}

/// Runs every Set-method protocol boundary under one dispatch and collection policy.
fn assert_set_methods_batch<const N: usize>(forced_major: bool) {
    for (index, (label, source)) in SET_METHOD_FIXTURES.into_iter().enumerate() {
        let module = compile_set_method_source(source, 8_600 + N as u32 * 10 + index as u32);
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
                    fuel: 262_144,
                    quantum: 262_144,
                },
            )
            .unwrap_or_else(|error| panic!("{label} fixture executes: {error:?}"));
        assert!(
            matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
            "{label}, dispatch batch {N}, forced_major={forced_major} returned {outcome:?}"
        );
    }
}

/// Compiles one Set-method fixture without coupling it to a collection policy.
fn compile_set_method_source(source: &str, source_id: u32) -> CompiledModule {
    Compiler
        .compile(
            SourceText::new(
                SourceId::new(source_id),
                SourceName::new("set-methods-fixture"),
                MediaType::JavaScript,
                Arc::from(source),
            ),
            CompileOptions::default(),
        )
        .expect("Set methods fixture compiles")
}
