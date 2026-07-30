use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{fixtures::test_isolate, *};

const SYMBOL_SOURCE: &str = r#"
var trace = "";
var first = { toString() { trace += "a"; return "A"; } };
var second = { [Symbol.toPrimitive](hint) { trace += hint; return "B"; } };
var registered = Symbol.for({ toString() { trace += "r"; return "registry"; } });
var symbol = Symbol({ toString() { trace += "s"; return "description"; } });
var concat = "".concat(first, second, "C", 4, true, null, undefined);
var boxed = Object(symbol);
var directConstructThrows = false;
try { new Symbol(); } catch (error) { directConstructThrows = error instanceof TypeError; }
var constructible = false;
try { Reflect.construct(function() {}, [], Symbol); constructible = true; } catch (_) {}
var original = Symbol.prototype[Symbol.toPrimitive];
delete Symbol.prototype[Symbol.toPrimitive];
var fallback = `${Object(Symbol("fallback"))}`;
Object.defineProperty(Symbol.prototype, Symbol.toPrimitive, {
  value: original,
  writable: false,
  configurable: true
});
symbol.description === "description" &&
boxed.valueOf() === symbol &&
Object.getPrototypeOf(symbol) === Symbol.prototype &&
Symbol.keyFor(registered) === "registry" &&
Symbol.for("registry") === registered &&
Symbol.prototype[Symbol.toStringTag] === "Symbol" &&
Object.getOwnPropertyDescriptor(Map, Symbol.species).get.name === "get [Symbol.species]" &&
Object.getOwnPropertyDescriptor(Set, Symbol.species).get.name === "get [Symbol.species]" &&
concat === "ABC4truenullundefined" &&
trace === "rsa" + "string" &&
fallback === "Symbol(fallback)" &&
directConstructThrows && constructible;
"#;

#[test]
fn symbol_surface_is_stable_for_every_dispatch_batch() {
    assert_symbol_source::<1>(false);
    assert_symbol_source::<2>(false);
    assert_symbol_source::<4>(false);
    assert_symbol_source::<8>(false);
    assert_symbol_source::<16>(false);
}

#[test]
fn symbol_conversion_state_survives_forced_major_collections() {
    assert_symbol_source::<8>(true);
}

/// Compiles and executes the Symbol/conversion fixture under one dispatch and GC policy.
fn assert_symbol_source<const N: usize>(forced_major: bool) {
    let module = Compiler
        .compile(
            SourceText::new(
                SourceId::new(2_900 + N as u32 + u32::from(forced_major)),
                SourceName::new("symbol-fixture"),
                MediaType::JavaScript,
                Arc::from(SYMBOL_SOURCE),
            ),
            CompileOptions::default(),
        )
        .expect("Symbol fixture compiles");
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
        .expect("Symbol fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N}, forced_major={forced_major} returned {outcome:?}"
    );
}
