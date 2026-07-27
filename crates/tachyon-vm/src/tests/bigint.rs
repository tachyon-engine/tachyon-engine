use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{fixtures::test_isolate, *};

const BIGINT_PRIMITIVE_SOURCE: &str = r#"
typeof 0n === "bigint"
    && !0n
    && !!1n
    && 140737488355327n === 140737488355327n
    && 140737488355328n === 140737488355328n
    && 140737488355328n !== 140737488355329n
    && (140737488355328n + "") === "140737488355328"
    && (340282366920938463463374607431768211455n + "")
        === "340282366920938463463374607431768211455"
    && -140737488355328n === -140737488355328n
    && (-18446744073709551617n + "") === "-18446744073709551617";
"#;

const BIGINT_CONVERSION_SOURCE: &str = r#"
var marker = {};
var trace = "";
var once = {
  [Symbol.toPrimitive](hint) {
    trace += hint;
    return "18446744073709551616";
  }
};
var getter = {};
Object.defineProperty(getter, Symbol.toPrimitive, {
  get() {
    trace += "g";
    return function(hint) { trace += hint; return true; };
  }
});
var thrownIdentity = false;
try {
  BigInt({ [Symbol.toPrimitive]() { throw marker; } });
} catch (error) {
  thrownIdentity = error === marker;
}
var constructCoerced = false;
var constructRejected = false;
try {
  new BigInt({ valueOf() { constructCoerced = true; return 1; } });
} catch (error) {
  constructRejected = error instanceof TypeError;
}
function throws(expected, callback) {
  try { callback(); } catch (error) { return error instanceof expected; }
  return false;
}
var signed = new BigInt64Array([
  "18446744073709551615",
  true,
  { valueOf() { trace += "v"; return "2"; } }
]);
var failure = 0;
if (!(BigInt.name === "BigInt" && BigInt.length === 1 &&
      constructRejected && !constructCoerced)) failure = 1;
if (!(BigInt(0n) === 0n && BigInt(-1n) === -1n &&
      BigInt(9007199254740994) === 9007199254740994n &&
      BigInt(-9007199254740994) === -9007199254740994n &&
      BigInt(false) === 0n && BigInt(true) === 1n)) failure = 2;
if (!(BigInt("") === 0n && BigInt("\u00a0\u2028123\u3000") === 123n &&
      BigInt("+42") === 42n && BigInt("-42") === -42n &&
      BigInt("0b1111") === 15n && BigInt("0O70") === 56n &&
      BigInt("0xfffffffffffffffffff") === 75557863725914323419135n)) failure = 3;
if (!(BigInt(once) === 18446744073709551616n && BigInt(getter) === 1n &&
      trace === "vnumbergnumber" && thrownIdentity)) failure = 4;
if (!(signed[0] === -1n && signed[1] === 1n && signed[2] === 2n)) failure = 5;
if (!(throws(TypeError, function() { new BigInt(1); }) &&
      throws(TypeError, function() { BigInt(); }) &&
      throws(TypeError, function() { BigInt(null); }) &&
      throws(TypeError, function() { BigInt(Symbol()); }) &&
      throws(RangeError, function() { BigInt(NaN); }) &&
      throws(RangeError, function() { BigInt(Infinity); }) &&
      throws(RangeError, function() { BigInt(1.5); }) &&
      throws(SyntaxError, function() { BigInt("10n"); }) &&
      throws(SyntaxError, function() { BigInt("-0x1"); }) &&
      throws(SyntaxError, function() { BigInt("0x"); }) &&
      throws(TypeError, function() { new BigInt64Array([1]); }))) failure = 6;
failure;
"#;

#[test]
fn bigint_primitives_execute_for_every_dispatch_batch() {
    assert_bigint_source::<1>(false);
    assert_bigint_source::<2>(false);
    assert_bigint_source::<4>(false);
    assert_bigint_source::<8>(false);
    assert_bigint_source::<16>(false);
}

#[test]
fn rooted_bigint_constants_survive_forced_major_collection() {
    assert_bigint_source::<1>(true);
    assert_bigint_source::<2>(true);
    assert_bigint_source::<4>(true);
    assert_bigint_source::<8>(true);
    assert_bigint_source::<16>(true);
}

#[test]
fn bigint_constructor_and_tobigint_execute_for_every_dispatch_batch() {
    assert_bigint_conversion::<1>(false);
    assert_bigint_conversion::<2>(false);
    assert_bigint_conversion::<4>(false);
    assert_bigint_conversion::<8>(false);
    assert_bigint_conversion::<16>(false);
}

#[test]
fn bigint_conversion_callbacks_survive_forced_major_collection() {
    assert_bigint_conversion::<1>(true);
    assert_bigint_conversion::<2>(true);
    assert_bigint_conversion::<4>(true);
    assert_bigint_conversion::<8>(true);
    assert_bigint_conversion::<16>(true);
}

/// Compiles and executes the primitive surface under one dispatch and collection policy.
fn assert_bigint_source<const N: usize>(forced_major: bool) {
    let module = Compiler
        .compile(
            SourceText::new(
                SourceId::new(2_400 + N as u32 + u32::from(forced_major) * 32),
                SourceName::new("bigint-primitive-fixture"),
                MediaType::JavaScript,
                Arc::from(BIGINT_PRIMITIVE_SOURCE),
            ),
            CompileOptions::default(),
        )
        .expect("BigInt primitive fixture compiles");
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
                fuel: 4_096,
                quantum: 4_096,
            },
        )
        .expect("BigInt primitive fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N}, forced_major={forced_major} returned {outcome:?}"
    );
}

/// Exercises constructor-only Number conversion and shared ToBigInt under one VM policy.
fn assert_bigint_conversion<const N: usize>(forced_major: bool) {
    let module = Compiler
        .compile(
            SourceText::new(
                SourceId::new(2_500 + N as u32 + u32::from(forced_major) * 32),
                SourceName::new("bigint-conversion-fixture"),
                MediaType::JavaScript,
                Arc::from(BIGINT_CONVERSION_SOURCE),
            ),
            CompileOptions::default(),
        )
        .expect("BigInt conversion fixture compiles");
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
                fuel: 16_384,
                quantum: 16_384,
            },
        )
        .expect("BigInt conversion fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(0)),
        "dispatch batch {N}, forced_major={forced_major} returned {outcome:?}"
    );
}
