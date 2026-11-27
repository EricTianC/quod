# Pure-`Prop` Tasks, Verified Overlays, and Merge Tasks (Quod)

In Quod, workers never write complex `Reduction` meta-connectors. Every worker task is a **closed `Prop`**:

1. **`Lemma` / `AuxLemma` / `Root`**: Prove `theorem <candidate_decl> : <Prop> := by ...`. Upon passing Stage 1, `quod-verifier` compiles your file to `.olean` and publishes it into `Verified Overlays`.
2. **`DefOverlay` (`I1`/`I2` IR)**: Pure definition modules (`def`, `structure`, `instance`) published via `PUBLISH_DEF_OVERLAY`, verified axiom-free, and importable by tasks.
3. **`Merge`**: Starts `Blocked` while `depends_on` lemmas are open. Once all upstream lemmas are `Proved`, `C0` automatically injects their `QuodOverlay_*` modules into `curated_imports` and transitions the `Merge` task to `Open`. Your job is simply to `import` those verified overlays and close the combined `Prop`!
4. **`Refutation`**: Prove `¬ <Prop>` (`--kind refutation`). Passing verification immediately marks the target `RefutedHard` and prunes its `Route`, while keeping already-proved `Overlays` reusable.

## Collision-Free Naming
When multiple parallel lemmas are imported together into a `Merge` task, each lemma must have a unique `candidate_decl` (provided in `TASK.md` / `<target_file>`) or wrap helpers in `private` / a task-specific `namespace` so Lean modules never clash.
