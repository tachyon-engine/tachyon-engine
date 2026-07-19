use super::fixtures::*;

#[test]
fn for_in_iterator_loop_is_stable_for_every_dispatch_batch() {
    assert_for_in_batch::<1>();
    assert_for_in_batch::<2>();
    assert_for_in_batch::<4>();
    assert_for_in_batch::<8>();
    assert_for_in_batch::<16>();
}

#[test]
fn logical_short_circuit_preserves_operands_for_every_dispatch_batch() {
    assert_logical_batch::<1>();
    assert_logical_batch::<2>();
    assert_logical_batch::<4>();
    assert_logical_batch::<8>();
    assert_logical_batch::<16>();
}

#[test]
fn switch_dispatch_chain_is_stable_for_every_dispatch_batch() {
    assert_switch_batch::<1>();
    assert_switch_batch::<2>();
    assert_switch_batch::<4>();
    assert_switch_batch::<8>();
    assert_switch_batch::<16>();
}

#[test]
fn catch_dispatch_and_cross_frame_throw_work_for_every_dispatch_batch() {
    assert_catch_batch::<1>();
    assert_catch_batch::<2>();
    assert_catch_batch::<4>();
    assert_catch_batch::<8>();
    assert_catch_batch::<16>();
}
