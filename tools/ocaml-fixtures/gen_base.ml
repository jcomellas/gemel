(* Golden test vectors for gemel-base's clroot, util and uarg modules,
   produced by Unison's own src/clroot.ml (built with the stand-ins in
   stubs/ for the parts of Util and Prefs it uses) and OCaml's stdlib.

   Usage: gen_base <fixtures-dir>
   Writes base.txt: tab-separated lines; inputs are hex ("-" when empty). *)

(* split_on_space, disallowed_char, quote and unquote from Unison's
   src/fswatch.ml, verbatim (watchercommon.ml has the same code):
   Derived from Unison, Copyright 1999-2020, Benjamin C. Pierce. *)
let split_on_space s =
  try
    let i = String.index s ' ' in
    (String.sub s 0 i,
     String.sub s (i + 1) (String.length s - i - 1))
  with Not_found ->
    (s, "")

let disallowed_char c =
  match c with
    'A'..'Z' | 'a'..'z' | '0'..'9' | '-' | '_' | '.' | '~'
  | '!' | '*' | '\'' | '(' | ')' | ';' | ':' | '@' | '&'
  | '=' | '+' | '$' | ',' | '/' | '?' | '#' | '[' | ']' ->
    false
  | _ ->
    true

let quote s =
  let l = String.length s in
  let n = ref 0 in
  for i = 0 to l - 1 do if disallowed_char s.[i] then incr n done;
  if !n = 0 then s else begin
    let q = Bytes.create (l + 2 * !n) in
    let j = ref 0 in
    let hex = "0123456789ABCDEF" in
    for i = 0 to l - 1 do
      let c = s.[i] in
      if disallowed_char s.[i] then begin
        Bytes.set q !j '%';
        Bytes.set q (!j + 1) hex.[Char.code c lsr 4];
        Bytes.set q (!j + 2) hex.[Char.code c land 15];
        j := !j + 3
      end else begin
        Bytes.set q !j c;
        incr j
      end
    done;
    Bytes.to_string q
  end

let unquote s =
  let l = String.length s in
  let n = ref 0 in
  for i = 0 to l - 1 do if s.[i] = '%' then incr n done;
  if !n = 0 then s else begin
    let hex_char c =
      match c with
        '0'..'9' -> Char.code c - Char.code '0'
      | 'a'..'f' -> Char.code c - Char.code 'a' + 10
      | 'A'..'F' -> Char.code c - Char.code 'A' + 10
      | _        -> invalid_arg "unquote"
    in
    let u = Bytes.create (l - 2 * !n) in
    let j = ref 0 in
    for i = 0 to l - 2 * !n - 1 do
      let c = s.[!j] in
      if c = '%' then begin
        Bytes.set u i (Char.chr ((hex_char s.[!j + 1]) lsl 4 + hex_char s.[!j + 2]));
        j := !j + 3
      end else begin
        Bytes.set u i c;
        incr j
      end
    done;
    Bytes.to_string u
  end

let hex s =
  if s = "" then "-" else
  String.concat ""
    (List.map (fun c -> Printf.sprintf "%02x" (Char.code c))
       (List.of_seq (String.to_seq s)))

let opt = function None -> "~" | Some s -> "=" ^ s

let show_root = function
  | Clroot.ConnectLocal p -> String.concat "\t" ["L"; opt p]
  | Clroot.ConnectByShell (sh, h, u, p, s) ->
      String.concat "\t" ["H"; sh; h; opt u; opt p; opt s]
  | Clroot.ConnectBySocket (h, p, s) -> String.concat "\t" ["S"; h; p; opt s]

let catch f x =
  try f x with
  | Util.Fatal m -> "E\tFatal\t" ^ m
  | Util.Transient m -> "E\tTransient\t" ^ m
  | Prefs.IllegalValue m -> "E\tIllegalValue\t" ^ m
  | Invalid_argument m -> "E\tInvalidArgument\t" ^ m

let roots = [
  "/home/a"; "a/b"; ""; "  /x  "; "."; "c:/winnt"; "file:foo";
  "//host"; "//host/"; "//host/path"; "//host:22/p"; "///abs/path";
  "file:///abs"; "file://h/p"; "file://u@h/p"; "file:/x"; "file:"; "file://u@/p";
  "ssh://h"; "ssh://h/"; "ssh://h/p"; "ssh://u@h/p"; "ssh://u@v@h:2222//abs";
  "ssh://@h/p"; "ssh://@@h/p"; "ssh:///p"; "ssh://h:22"; "ssh://[::1]/p";
  "ssh://[fe80::1%en0]:22/p"; "ssh://[FE80::1]/p"; "ssh://[::1%]/p";
  "ssh://[::1"; "ssh://h:/p"; "ssh://h:2:3/p"; "ssh://h:abc/p";
  "ssh://h%20x/p"; "ssh://h p"; "ssh:h"; "ssh://h/p/ q"; "ssh://u.v-w_x%y@h/p";
  "socket://h:4040/p"; "socket://h/p"; "socket://u@h:1/p";
  "socket://{/tmp/s}/p"; "socket://{/tmp/s}:1/p"; "socket://{}/p";
  "socket://{/tmp/s"; "socket://[::1]:4040/"; "socket://:4040/p"; "socket:";
  "rsh://h/p"; "unison://h/p"; "RSH://saul/hello"; "rsh:/saul/hello";
  "http://x/y"; "x:y"; "//saul:40a4/hello"; "//s%aul/hello";
  "socket://tjim@saul.cis.upenn.edu:4040/hello/world";
  "ssh://tjim@saul/hello/world"; "//raptor/usr/local/bin";
  "file://raptor/usr/local/bin"; "//turtle/c:/winnt/";
  "file://turtle/c:/winnt/" ]

(* Roots as fixHost sees them: built from separate host settings. *)
let fix_hosts = [
  Clroot.ConnectByShell ("ssh", "h:22", None, None, Some "p");
  Clroot.ConnectByShell ("ssh", "h", Some "u", None, None);
  Clroot.ConnectByShell ("ssh", "h:22", None, Some "1", None);
  Clroot.ConnectByShell ("ssh", "::1", None, None, None);
  Clroot.ConnectByShell ("ssh", "[::1]:2", None, None, None);
  Clroot.ConnectByShell ("ssh", "h:2/x", None, None, None);
  Clroot.ConnectBySocket ("h:4040", "", None);
  Clroot.ConnectBySocket ("{/s}", "", Some "p");
  Clroot.ConnectBySocket ("h", "", None);
  Clroot.ConnectBySocket ("h", "1", None);
  Clroot.ConnectLocal (Some "x") ]

let ints = [
  "0"; "42"; "-7"; "+3"; "1_000"; "_1"; "0x1F"; "0X1f"; "0o17"; "0O17";
  "0b101"; "0B11"; "0u5"; "-0x10"; "4611686018427387903";
  "4611686018427387904"; "-4611686018427387904"; "-4611686018427387905";
  "0x3fffffffffffffff"; "0x4000000000000000"; "0x7fffffffffffffff";
  "0x8000000000000000"; "-0x7fffffffffffffff"; "0u4611686018427387904";
  "0u9223372036854775807"; "9223372036854775807"; ""; " 1"; "1 "; "0x";
  "1e3"; "1.0"; "--1"; "-_1"; "0_"; "1__2"; "0b2"; "0o8"; "0xg"; "-0";
  "0123"; "+"; "-"; "0x_1"; "0_x1" ]

let words = [
  "include a"; "  include  a  "; "include a\\ b"; "a\\"; "\\\\"; "";
  "   "; "a\tb"; "x\\ny"; "include a b" ]

let trims = [ " a "; "\t\r\n a b \n"; ""; "   "; "a"; "\x0ba\x0b" ]

let quotes = [
  ""; "abc"; "a b"; "a\nb"; "%"; "/home/me/My Docs"; "caf\xc3\xa9";
  "-_.~!*'();:@&=+$,/?#[]"; "\x00\x01\x7f\x80\xff"; "\"<>\\^`{|}";
  String.init 256 Char.chr ]

let unquotes = [
  ""; "abc"; "a%20b"; "%41"; "%4a%4A"; "%e9"; "%"; "%4"; "a%4"; "a%"; "%%";
  "%zz"; "%4g"; "%41%"; "%4%41"; "x%2"; "100%25"; "%0A%0D" ]

let splits = [ ""; "OK"; "DIR a%20b"; "START h /r p"; "CHANGES "; " x"; "a  b" ]

let () =
  let oc = open_out_bin (Filename.concat Sys.argv.(1) "base.txt") in
  Printf.fprintf oc
    "# gemel-base golden vectors, generated by tools/ocaml-fixtures/gen_base.ml.\n\
     # Unison source: %s. OCaml %s, %d-bit.\n\
     # Regenerate with scripts/gen-ocaml-fixtures.sh; do not edit by hand.\n\
     # Tab-separated. Inputs are hex (\"-\" when empty).\n\
     # root <in> <parseRoot> | <clroot2string> | <fixHost>\n\
     # fix <fixHost input> <fixHost>\n\
     # int <in> <int_of_string, or !>\n\
     # words <in> <splitIntoWords on ' ', as hex joined by ','>\n\
     # trim <in> <trimWhitespace, hex>\n\
     # quote <in> <fswatch quote, hex>\n\
     # unquote <in> <fswatch unquote, hex, or !>\n\
     # split <in> <split_on_space: first, hex> <rest, hex>\n"
    (Option.value (Sys.getenv_opt "UNISON_COMMIT") ~default:"unknown")
    Sys.ocaml_version Sys.word_size;
  List.iter (fun s ->
      let parsed = try Ok (Clroot.parseRoot s) with e -> Error e in
      let result = match parsed with
        | Ok r ->
            String.concat "\t|\t"
              [ show_root r; Clroot.clroot2string r;
                catch (fun r -> show_root (Clroot.fixHost r)) r ]
        | Error e -> catch (fun () -> raise e) ()
      in
      Printf.fprintf oc "root\t%s\t%s\n" (hex s) result) roots;
  List.iter (fun r ->
      Printf.fprintf oc "fix\t%s\t%s\n" (show_root r)
        (catch (fun r -> show_root (Clroot.fixHost r)) r)) fix_hosts;
  List.iter (fun s ->
      Printf.fprintf oc "int\t%s\t%s\n" (hex s)
        (match int_of_string_opt s with Some n -> string_of_int n | None -> "!"))
    ints;
  List.iter (fun s ->
      Printf.fprintf oc "words\t%s\t%s\n" (hex s)
        (String.concat "," (List.map hex (Util.splitIntoWords s ' ')))) words;
  List.iter (fun s ->
      Printf.fprintf oc "trim\t%s\t%s\n" (hex s) (hex (Util.trimWhitespace s)))
    trims;
  List.iter (fun s ->
      Printf.fprintf oc "quote\t%s\t%s\n" (hex s) (hex (quote s))) quotes;
  List.iter (fun s ->
      Printf.fprintf oc "unquote\t%s\t%s\n" (hex s)
        (try hex (unquote s) with Invalid_argument _ -> "!")) unquotes;
  List.iter (fun s ->
      let (a, b) = split_on_space s in
      Printf.fprintf oc "split\t%s\t%s\t%s\n" (hex s) (hex a) (hex b)) splits;
  close_out oc
