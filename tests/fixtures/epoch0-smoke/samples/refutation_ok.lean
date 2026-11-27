import Spec

theorem candidate : ¬ Quod.Epoch0.false_stmt := by
  intro h
  have h0 := h 0
  omega
