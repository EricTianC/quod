import Spec
import QuodDef_01
import QuodOverlay_a
import QuodOverlay_b

theorem candidate : Quod.Epoch0.smoke_stmt := by
  intro n
  have h1 := proof_lemma_a n
  have h2 := proof_lemma_b n
  rw [h1, h2]
