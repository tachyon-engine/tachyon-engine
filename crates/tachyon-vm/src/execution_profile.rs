//! Opt-in interpreter counters used to choose structural optimizations from dynamic evidence.

use tachyon_bytecode::Opcode;

/// Dynamic counts for one semantic opcode.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct OpcodeExecutionCounts {
    /// Instructions decoded and attempted.
    pub executed: u64,
    /// Instructions completed inside the verified no-GC kernel.
    pub hot: u64,
    /// Instructions that exited to the generic semantic dispatcher.
    pub slow: u64,
    /// Conditional branches whose selected successor differs from fallthrough.
    pub branch_taken: u64,
    /// Conditional branches that retained their fallthrough successor.
    pub branch_not_taken: u64,
}

/// Per-isolate diagnostic counters; this type and all update sites disappear without the feature.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutionProfile {
    opcodes: [OpcodeExecutionCounts; Opcode::COUNT],
    kernel_cursor_binds: u64,
    poll_groups: u64,
    budget_flushes: u64,
    slow_flushes: u64,
    slow_rebinds: u64,
    same_activation_rebinds: u64,
    activation_rebinds: u64,
    terminal_slow_exits: u64,
    fault_slow_exits: u64,
}

impl ExecutionProfile {
    /// Returns the counts for one opcode.
    #[must_use]
    pub const fn opcode(&self, opcode: Opcode) -> OpcodeExecutionCounts {
        self.opcodes[opcode as usize]
    }

    /// Iterates every opcode in bytecode index order, including zero-count entries.
    pub fn opcodes(&self) -> impl ExactSizeIterator<Item = (Opcode, OpcodeExecutionCounts)> + '_ {
        self.opcodes.iter().enumerate().map(|(index, counts)| {
            (
                Opcode::from_index(index).expect("profile indices cover dense opcodes"),
                *counts,
            )
        })
    }

    /// Returns cursor binds performed once at kernel entry before any slow rebind.
    #[must_use]
    pub const fn kernel_cursor_binds(&self) -> u64 {
        self.kernel_cursor_binds
    }

    /// Returns complete N-instruction polling groups crossed without publishing the local PC.
    #[must_use]
    pub const fn poll_groups(&self) -> u64 {
        self.poll_groups
    }

    /// Returns the number of PC publications caused by an exhausted bounded budget.
    #[must_use]
    pub const fn budget_flushes(&self) -> u64 {
        self.budget_flushes
    }

    /// Returns the number of PC publications before generic semantic dispatch.
    #[must_use]
    pub const fn slow_flushes(&self) -> u64 {
        self.slow_flushes
    }

    /// Returns the number of continued slow exits that rebuilt a verified cursor.
    #[must_use]
    pub const fn slow_rebinds(&self) -> u64 {
        self.slow_rebinds
    }

    /// Returns slow rebinds that resumed the same code, function, and register base.
    #[must_use]
    pub const fn same_activation_rebinds(&self) -> u64 {
        self.same_activation_rebinds
    }

    /// Returns slow rebinds that entered or returned to a different activation.
    #[must_use]
    pub const fn activation_rebinds(&self) -> u64 {
        self.activation_rebinds
    }

    /// Returns slow exits that completed execution instead of rebuilding a cursor.
    #[must_use]
    pub const fn terminal_slow_exits(&self) -> u64 {
        self.terminal_slow_exits
    }

    /// Returns slow exits that ended in an engine fault before a language outcome was produced.
    #[must_use]
    pub const fn fault_slow_exits(&self) -> u64 {
        self.fault_slow_exits
    }

    pub(crate) fn record_instruction(&mut self, opcode: Opcode, hot: bool) {
        let counts = &mut self.opcodes[opcode as usize];
        counts.executed = counts.executed.saturating_add(1);
        if hot {
            counts.hot = counts.hot.saturating_add(1);
        } else {
            counts.slow = counts.slow.saturating_add(1);
        }
    }

    pub(crate) fn record_branch(&mut self, opcode: Opcode, taken: bool) {
        debug_assert!(matches!(
            opcode,
            Opcode::JumpIfFalse | Opcode::JumpIfTrue | Opcode::JumpIfNotNullish
        ));
        let counts = &mut self.opcodes[opcode as usize];
        if taken {
            counts.branch_taken = counts.branch_taken.saturating_add(1);
        } else {
            counts.branch_not_taken = counts.branch_not_taken.saturating_add(1);
        }
    }

    pub(crate) fn record_kernel_cursor_bind(&mut self) {
        self.kernel_cursor_binds = self.kernel_cursor_binds.saturating_add(1);
    }

    pub(crate) fn record_poll_group(&mut self) {
        self.poll_groups = self.poll_groups.saturating_add(1);
    }

    pub(crate) fn record_budget_flush(&mut self) {
        self.budget_flushes = self.budget_flushes.saturating_add(1);
    }

    pub(crate) fn record_slow_flush(&mut self) {
        self.slow_flushes = self.slow_flushes.saturating_add(1);
    }

    pub(crate) fn record_slow_rebind(&mut self, activation_changed: bool) {
        self.slow_rebinds = self.slow_rebinds.saturating_add(1);
        if activation_changed {
            self.activation_rebinds = self.activation_rebinds.saturating_add(1);
        } else {
            self.same_activation_rebinds = self.same_activation_rebinds.saturating_add(1);
        }
    }

    pub(crate) fn record_terminal_slow_exit(&mut self) {
        self.terminal_slow_exits = self.terminal_slow_exits.saturating_add(1);
    }

    pub(crate) fn record_fault_slow_exit(&mut self) {
        self.fault_slow_exits = self.fault_slow_exits.saturating_add(1);
    }
}

impl Default for ExecutionProfile {
    fn default() -> Self {
        Self {
            opcodes: [OpcodeExecutionCounts::default(); Opcode::COUNT],
            kernel_cursor_binds: 0,
            poll_groups: 0,
            budget_flushes: 0,
            slow_flushes: 0,
            slow_rebinds: 0,
            same_activation_rebinds: 0,
            activation_rebinds: 0,
            terminal_slow_exits: 0,
            fault_slow_exits: 0,
        }
    }
}
