(* Stand-in for Unison's src/trace.ml, for building transfer.ml in
   gen_transfer.ml: debugging and timers are off. *)

let debug (_ : string) (_ : unit -> unit) = ()
let enabled (_ : string) = false

type timer = unit
let startTimer (_ : string) = ()
let showTimer () = ()
