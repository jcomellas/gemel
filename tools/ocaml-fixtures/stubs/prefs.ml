(* Stand-in for Unison's src/ubase/prefs.ml, for building clroot.ml in
   gen_base.ml and pred.ml in gen_pred.ml: only what they use. A
   preference is a reference to its value. *)

exception IllegalValue of string

let markRemoved (_ : string) = ()

type 'a t = 'a ref

let create (_ : string) ~category:_ ?local:_ ?send:_ initial (_ : string)
    (_ : string) (_ : 'a -> string -> 'a) (_ : 'a -> string list) _ =
  ref initial

let read p = !p
let set p v = p := v
let alias _ (_ : string) = ()
