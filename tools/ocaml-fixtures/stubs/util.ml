(* Stand-in for Unison's src/ubase/util.ml, for building clroot.ml in
   gen_base.ml. The functions below are copied verbatim from util.ml:
   Derived from Unison, Copyright 1999-2020, Benjamin C. Pierce. *)

exception Fatal of string
exception Transient of string

let removeTrailingCR s =
  let l = String.length s in
  if l = 0 || s.[l - 1] <> '\r' then s else
  String.sub s 0 (l - 1)

let trimWhitespace s =
  let l = String.length s in
  let rec loop lp rp =
    if lp > rp then ""
    else if s.[lp]=' ' || s.[lp]='\t' || s.[lp]='\n' || s.[lp]='\r' then
      loop (lp+1) rp
    else if s.[rp]=' ' || s.[rp]='\t' || s.[rp]='\n' || s.[rp]='\r' then
      loop lp (rp-1)
    else
      String.sub s lp (rp+1-lp)
   in
   loop 0 (l-1)

let splitAtChar ?reverse:(rev=false) (s:string) (c:char) =
  try
    let i = if rev then String.rindex s c else String.index s c
    and l = String.length s in
    (* rest is possibly the empty string *)
    (String.sub s 0 i, Some (String.sub s (i+1) (l-i-1)))
  with Not_found -> (s, None)

let splitIntoWords ?esc:(e='\\') (s:string) (c:char) =
  let rec inword acc eacc start pos =
    if pos >= String.length s || s.[pos] = c then
      let word =
        String.concat "" (Safelist.rev (String.sub s start (pos-start)::eacc)) in
      betweenwords (word::acc) pos
    else if s.[pos] = e then inescape acc eacc start pos
    else inword acc eacc start (pos+1)
  and inescape acc eacc start pos =
    let eword = String.sub s start (pos-start) in
    if pos+1 >= String.length s
    then inword acc (eword::eacc) (pos+1) (pos+1) (* ignore final esc *)
    else (* take any following char *)
      let echar = String.make 1 (String.get s (pos+1)) in
      inword acc (echar::eword::eacc) (pos+2) (pos+2)
  and betweenwords acc pos =
    if pos >= String.length s then (Safelist.rev acc)
    else if s.[pos]=c then betweenwords acc (pos+1)
    else inword acc [] pos pos
  in betweenwords [] 0

