(* Stand-in for Unison's src/ubase/prefs.ml, for building clroot.ml in
   gen_base.ml: only what clroot.ml uses. *)

exception IllegalValue of string

let markRemoved (_ : string) = ()
