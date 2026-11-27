/-
Quod Transitive Slice Extractor (M1 skeleton).
Walks `Lean.Environment` dependencies for a target declaration inside a locked Epoch
so `quod-hub` can emit deterministic `GapPacket.transitive_slice` payloads.
-/
import Lean

namespace Quod.Harness

open Lean

/-- Collects direct constant references appearing in a declaration's type and value. -/
def directConstants (ci : ConstantInfo) : NameSet :=
  ci.type.getUsedConstants.foldl NameSet.insert NameSet.empty

/-- Computes the transitive declaration closure within the current environment. -/
partial def transitiveDeps (env : Environment) (roots : List Name) : NameSet :=
  let rec go (queue : List Name) (visited : NameSet) : NameSet :=
    match queue with
    | [] => visited
    | nm :: rest =>
      if visited.contains nm then
        go rest visited
      else
        let visited' := visited.insert nm
        match env.find? nm with
        | some ci =>
          let next := (directConstants ci).toList
          go (next ++ rest) visited'
        | none =>
          go rest visited'
  go roots NameSet.empty

end Quod.Harness
