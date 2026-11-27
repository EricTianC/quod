import Mathlib.Data.Int.ModEq
import Mathlib.Data.Set.Finite.Basic

namespace Quod.Epoch1

/-- Erdős Problem 479: for every integer `k ≠ 1`, there are infinitely many `n : ℕ`
such that `2 ^ n ≡ k [ZMOD n]`. -/
def erdos_479_stmt : Prop :=
  ∀ k : ℤ, k ≠ 1 → { n : ℕ | (2 : ℤ) ^ n ≡ k [ZMOD (n : ℤ)] }.Infinite

end Quod.Epoch1
