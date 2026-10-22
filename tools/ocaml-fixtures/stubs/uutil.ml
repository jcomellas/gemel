(* Stand-in for Unison's src/uutil.ml, for building transfer.ml in
   gen_transfer.ml: Filesize as in uutil.ml, where it is Int64.
   Derived from Unison, Copyright 1999-2020, Benjamin C. Pierce. *)

module Filesize = struct
  type t = int64
  let zero = 0L
  let dummy = -1L
  let add = Int64.add
  let sub = Int64.sub
  let ofFloat = Int64.of_float
  let toFloat = Int64.to_float
  let toString = Int64.to_string
  let ofInt x = Int64.of_int x
  let ofInt64 x = x
  let toInt x = Int64.to_int x
  let toInt64 x = x
end
