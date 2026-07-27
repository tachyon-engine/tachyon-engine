use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{fixtures::test_isolate, *};

const SEARCH_FIXTURE: &str = r#"
var basic = 'abc'.search(/b/) === 1 && 'abc'.search(/z/) === -1;
var customOrder = '';
var customPattern = {};
Object.defineProperty(customPattern, Symbol.search, {
  get: function() {
    customOrder += 'g';
    return function(value) {
      customOrder += this === customPattern && value === receiver ? 'c' : 'x';
      return 41;
    };
  }
});
var receiver = { toString: function() { customOrder += 's'; return 'unused'; } };
var custom = String.prototype.search.call(receiver, customPattern) === 41 && customOrder === 'gc';

var restore = /b/g;
restore.lastIndex = 7;
var restored = RegExp.prototype[Symbol.search].call(restore, 'abc') === 1 &&
  restore.lastIndex === 7;

var execOrder = '';
var searchReceiver = { lastIndex: 3 };
Object.defineProperty(searchReceiver, 'exec', {
  get: function() {
    execOrder += 'e';
    return function(value) {
      execOrder += this === searchReceiver && value === 'abc' ? 'c' : 'x';
      this.lastIndex = 9;
      return { get index() { execOrder += 'i'; return 2; } };
    };
  }
});
var customExec = RegExp.prototype[Symbol.search].call(searchReceiver, 'abc') === 2 &&
  searchReceiver.lastIndex === 3 && execOrder === 'eci';

var lifecycleOrder = '';
var storedLastIndex = -0;
var lifecycleReceiver = { exec: function() {
  lifecycleOrder += 'e';
  storedLastIndex = 4;
  return null;
} };
Object.defineProperty(lifecycleReceiver, 'lastIndex', {
  get: function() { lifecycleOrder += 'g'; return storedLastIndex; },
  set: function(value) {
    lifecycleOrder += Object.is(value, -0) ? 'r' : 'z';
    storedLastIndex = value;
  }
});
var lifecycleInput = { toString: function() { lifecycleOrder += 's'; return 'abc'; } };
var lifecycle = RegExp.prototype[Symbol.search].call(lifecycleReceiver, lifecycleInput) === -1 &&
  Object.is(storedLastIndex, -0) && lifecycleOrder === 'sgzegr';

var conversionError = {};
var conversionAbrupt = false;
try {
  RegExp.prototype[Symbol.search].call(/./, {
    toString: function() { throw conversionError; }
  });
} catch (error) {
  conversionAbrupt = error === conversionError;
}

Object.defineProperty(Number.prototype, Symbol.search, {
  get: function() { throw new Error('primitive @@search must not be observed'); }
});
var primitiveProtocol = 'primitive receiver 7'.search(7) === 19;

basic && custom && restored && customExec && lifecycle && conversionAbrupt && primitiveProtocol &&
String.prototype.search.name === 'search' && String.prototype.search.length === 1 &&
RegExp.prototype[Symbol.search].name === '[Symbol.search]' &&
RegExp.prototype[Symbol.search].length === 1;
"#;

fn assert_search<const N: usize>(force_major: bool) {
    assert_search_source::<N>("full", SEARCH_FIXTURE, force_major);
}

/// Compiles and runs one named search protocol fixture under a selected VM policy.
fn assert_search_source<const N: usize>(name: &str, source: &'static str, force_major: bool) {
    let module = Compiler
        .compile(
            SourceText::new(
                SourceId::new(8_100 + N as u32 + u32::from(force_major) * 32),
                SourceName::new("regexp-search-fixture"),
                MediaType::JavaScript,
                Arc::from(source),
            ),
            CompileOptions::default(),
        )
        .expect("search fixture compiles");
    let mut isolate = test_isolate();
    if force_major {
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
        .unwrap_or_else(|error| panic!("{name} search fixture executes: {error:?}"));
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "{name}: dispatch batch {N}, forced_major={force_major} returned {outcome:?}"
    );
}

#[test]
fn regexp_and_string_search_cover_dispatch_and_gc_matrix() {
    assert_search_source::<1>("string-basic", "'abc'.search(/b/) === 1;", false);
    assert_search_source::<1>(
        "regexp-basic",
        "RegExp.prototype[Symbol.search].call(/b/, 'abc') === 1;",
        false,
    );
    assert_search::<1>(false);
    assert_search::<2>(false);
    assert_search::<4>(false);
    assert_search::<8>(false);
    assert_search::<16>(false);
    assert_search::<8>(true);
}
