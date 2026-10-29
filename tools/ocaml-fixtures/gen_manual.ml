(* Unison's manual as Rust source, for gemel-ui-api's `manual` module,
   from the list that OCaml Unison itself prints for `-doc`.

   Linked against Unison's src/strings.ml (see
   scripts/gen-ocaml-fixtures.sh).

   Usage: gen_manual <manual-dir>
   Writes <manual-dir>/docs.rs: each of `Strings.docs`'s sections, in
   order, as a short name, a name and a raw string literal of its text.

   Derived from Unison, Copyright 1999-2020, Benjamin C. Pierce. *)

let commit () =
  Option.value (Sys.getenv_opt "UNISON_COMMIT") ~default:"unknown"

(* A string literal for a name: quotes and backslashes escaped. *)
let rust_string s =
  let b = Buffer.create (String.length s + 2) in
  Buffer.add_char b '"';
  String.iter (fun c ->
      match c with
      | '"' | '\\' -> Buffer.add_char b '\\'; Buffer.add_char b c
      | c when Char.code c < 0x20 -> Printf.bprintf b "\\x%02x" (Char.code c)
      | c -> Buffer.add_char b c) s;
  Buffer.add_char b '"';
  Buffer.contents b

(* A raw string literal for a text, with as many hashes as it takes for
   the text not to end it. *)
let rust_raw_string s =
  let contains sub =
    let n = String.length sub and m = String.length s in
    let rec at i = i + n <= m && (String.sub s i n = sub || at (i + 1)) in
    at 0
  in
  let rec hashes h = if contains ("\"" ^ h) then hashes (h ^ "#") else h in
  let h = hashes "#" in
  Printf.sprintf "r%s\"%s\"%s" h s h

let docs oc =
  Printf.fprintf oc
    "// Unison's manual, from src/strings.ml (%s), extracted by\n\
     // tools/ocaml-fixtures/gen_manual.ml.\n\
     // Regenerate with scripts/gen-ocaml-fixtures.sh; do not edit by hand.\n\
     //\n\
     // Derived from Unison, Copyright 1999-2020, Benjamin C. Pierce.\n\
     \n\
     use super::Section;\n\
     \n\
     #[rustfmt::skip]\n\
     pub(super) static DOCS: &[Section] = &[\n"
    (commit ());
  List.iter (fun (short, (name, text)) ->
      Printf.fprintf oc
        "    Section {\n\
        \        short: %s,\n\
        \        name: %s,\n\
        \        text: %s,\n\
        \    },\n"
        (rust_string short) (rust_string name) (rust_raw_string text))
    Strings.docs;
  Printf.fprintf oc "];\n"

let () =
  let dir = Sys.argv.(1) in
  let oc = open_out_bin (Filename.concat dir "docs.rs") in
  docs oc;
  close_out oc
