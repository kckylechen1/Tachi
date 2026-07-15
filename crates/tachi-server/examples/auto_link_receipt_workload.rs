//! auto_link_receipt_workload.rs — tachi#1097 S2 receipt workload harness (W5).
//!
//! # STATUS: BLOCKED — this file does NOT implement W5. It compiles and, when
//! run, prints a structured conflict report. Read on.
//!
//! W5 ("auto-link on save — three variants: 1, 5, 20 entities per saved entry,
//! env gate on") requires driving saves through the real server so
//! `spawn_auto_linking` fires, then polling for completion the way
//! `crates/tachi-server/src/tests/memory_tests/save_policy/auto_link.rs` does.
//! Every piece of machinery that contract calls for is `pub(crate)` in
//! `tachi-server` and therefore **unreachable from an example**. A Cargo
//! example is compiled as a *separate crate* that depends on the library; it
//! can name only `pub` items, never `pub(crate)` ones (the existing
//! `memcore/examples/split_antigravity.rs` confirms the convention — it uses
//! only public `MemoryStore` methods).
//!
//! ## The exact unreachable surface (file:line)
//!
//!   * `MemoryServer`                  — `pub(crate) struct`  at `src/server_state/tachi_server.rs:10`
//!   * `DbScope`                       — re-exported `pub(crate) use` at `src/lib.rs:178`
//!   * `MemoryServer::new`             — `pub(crate) fn`     at `src/server_state/init.rs:88`
//!   * `MemoryServer::save_memory`     — `pub(crate) async fn` at `src/tools/memory_facade.rs:22`
//!   * `MemoryServer::with_global_store`     — `pub(crate) fn` at `src/server_methods/db.rs:33`
//!   * `MemoryServer::with_global_store_read`— `pub(crate) fn` at `src/server_methods/db.rs:40`
//!   * `spawn_auto_linking`            — `pub(crate) fn`     at `src/memory_search_ops/auto_link.rs:365`
//!   * `run_auto_linking`              — `pub(crate) fn`     at `src/memory_search_ops/auto_link.rs:454`
//!   * `AutoLinkReceipt`               — `pub struct` at `auto_link.rs:70`, BUT its parent module
//!                                       `memory_search_ops` is a private `mod` (`src/lib.rs:133`),
//!                                       so the type is not nameable from outside the crate.
//!   * `Parameters` / `SaveMemoryParams`— live in `tool_params`, a private `mod` (`src/lib.rs:163`).
//!
//! The only `pub` items `tachi_server` exposes are `run_cli`, `ensure_tls_provider`,
//! `pub mod build_info`, and `pub mod exec_env_postflight` (the last is "NOT
//! WIRED YET" per its own doc). None construct a server, drive a save, or
//! surface the auto-link receipt.
//!
//! ## What would unblock W5
//!
//! This is a contract/allowlist question, not a code question I can solve
//! inside the allowlist ("create exactly these two files, edit nothing that
//! exists"). The honest options, for the contract owner:
//!
//!   1. Widen the allowlist so an existing file may change: promote the
//!      surface above from `pub(crate)` to `pub` (and make `memory_search_ops`
//!      + `server_state` + `tools` + `server_methods` + `tool_params` at least
//!      `pub` so the paths resolve). This leaks the internal server API, which
//!      is presumably why it is `pub(crate)` today.
//!   2. Add a single focused `pub` benchmark entrypoint on `MemoryServer`
//!      (e.g. a `pub fn run_auto_link_workload(...)` that the example calls).
//!      Smaller surface leak, but still an existing-file edit outside the
//!      allowlist.
//!   3. Move W5 into an integration test under `tests/` with `#[cfg(test)]`
//!      access to `pub(crate)` items, instead of an `examples/` file. That
//!      changes the deliverable shape, not just an edit.
//!
//! Per the contract ("If any part of this contract conflicts with the code you
//! find: STOP on that part, report the conflict, finish the parts that don't
//! conflict"), W1–W4 are delivered in
//! `crates/memcore/examples/receipts_workloads.rs` against verified PUBLIC
//! `memcore` APIs; W5 is STOPPED here.
//!
//! # How to run
//!
//!     cargo run -p tachi-server --example auto_link_receipt_workload
//!
//! Prints one JSONL object to stdout describing the block (so the build seat's
//! pipe receives a structured record, not a panic) and a detailed conflict
//! report to stderr.

fn main() {
    // The contract asks for the gate to be set at the top of main() before
    // the server is constructed. We honor that here even though no server can
    // be constructed from an example — so that the moment the visibility gap
    // above is closed, this line is already correct and in place.
    std::env::set_var("TACHI_AUTO_LINK_PHASE_RECEIPTS", "1");

    // Machine-readable record for the build-seat pipe.
    println!(
        "{{\"workload\":\"W5\",\"status\":\"BLOCKED\",\
         \"reason\":\"auto-link server surface is pub(crate); unreachable from an example\",\
         \"blocked_items\":[\
         \"MemoryServer@server_state/tachi_server.rs:10\",\
         \"DbScope@lib.rs:178\",\
         \"MemoryServer::new@server_state/init.rs:88\",\
         \"save_memory@tools/memory_facade.rs:22\",\
         \"with_global_store@server_methods/db.rs:33\",\
         \"with_global_store_read@server_methods/db.rs:40\",\
         \"spawn_auto_linking@memory_search_ops/auto_link.rs:365\",\
         \"run_auto_linking@memory_search_ops/auto_link.rs:454\",\
         \"AutoLinkReceipt@memory_search_ops/auto_link.rs:70\",\
         \"Parameters/SaveMemoryParams@tool_params(lib.rs:163)\"\
         ],\
         \"env_gate\":\"TACHI_AUTO_LINK_PHASE_RECEIPTS=1 set at main() top (ready for when the gap closes)\"}}"
    );

    eprintln!("┌─ W5 BLOCKED ──────────────────────────────────────────────────────────");
    eprintln!("│ W5 cannot be implemented as crates/tachi-server/examples/* without");
    eprintln!("│ editing an existing file: every API the contract names for W5 is");
    eprintln!("│ pub(crate), and an example is a separate crate that can only name pub");
    eprintln!("│ items. See the module-level doc of this file for the file:line list and");
    eprintln!("│ the three options that would unblock it. W1–W4 are unaffected and live");
    eprintln!("│ in crates/memcore/examples/receipts_workloads.rs.");
    eprintln!("└────────────────────────────────────────────────────────────────────────");
}
