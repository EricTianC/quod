namespace Quod.Epoch0

def smoke_stmt : Prop :=
  ∀ n : Nat, (n + 1) + (n + 1) = 2 * n + 2

def false_stmt : Prop :=
  ∀ n : Nat, n + 1 = n

end Quod.Epoch0
