# Issue #77 — superseded by #150

The **background wiki compilation pipeline** (Karpathy LLM Wiki nightly compile) is **not** the target architecture.

Use instead:

- `tachi_workflow(action=close_loop)` — write wiki at **issue close** with `references[]` linking Issue ↔ Doc ↔ Memory
- `tachi_wiki` / `tachi_wiki_write` with `references` (#149)

See [#150](https://github.com/kckylechen1/tachi/issues/150) and `tachi_workflow` tool docs.