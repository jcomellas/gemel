(* Stand-ins that the pieces of Unison's util.ml and uicommon.ml copied
   into gen_uicommon.ml need (see scripts/gen-ocaml-fixtures.sh). *)

module Uutil = struct
  module Filesize = struct
    type t = int64
    let toInt64 (x : t) = x
  end
end

(* Pred.mapSeparator, copied from pred.ml by the script. *)
module Pred = struct
