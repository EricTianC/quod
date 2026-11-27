import QuodDef_01

theorem proof_lemma_a : ∀ n : Nat, (n + 1) + (n + 1) = doublePlusTwo n := by
  intro n
  dsimp [doublePlusTwo]
  omega
