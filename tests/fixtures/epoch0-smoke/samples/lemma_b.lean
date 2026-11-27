import QuodDef_01

theorem proof_lemma_b : ∀ n : Nat, doublePlusTwo n = 2 * n + 2 := by
  intro n
  dsimp [doublePlusTwo]
  omega
