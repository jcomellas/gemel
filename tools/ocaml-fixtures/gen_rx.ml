(* Golden test vectors for gemel-base's rx module, produced by Unison's own
   src/ubase/rx.ml.

   Usage:
     gen_rx <fixtures-dir>        writes rx.txt (everything without ^ or $)
     gen_rx --anchors             lists the anchored patterns, as hex
     gen_rx --anchor <hex>        prints the lines for one anchored pattern

   Rx memoises derivatives in global tables whose keys leave out the line
   position, so with ^ or $ a result can depend on the regular expressions
   compiled before it in the same process. The script therefore runs each
   anchored pattern in a fresh process (--anchor), and the main corpus has
   no anchors outside brackets.

   Lines are tab-separated; strings are hex ("-" when empty):
     rx    <pattern> <input> <match_string> <match_substring> <prefix@0> <prefix@1>
     glob  <nodot> <globx> <pattern> <input> <match_string> <match_substring> <prefix@0>
     ci    <pattern> <input> <match_string of case_insensitive (rx pattern)>
   A pattern that fails to parse has one line with "E:Parse_error",
   "E:Not_supported" or "E:Invalid_argument" in place of the input and
   results. Booleans are 0/1;
   prefixes are a length or "~". *)

let hex s =
  if s = "" then "-" else
  String.concat ""
    (List.map (fun c -> Printf.sprintf "%02x" (Char.code c))
       (List.of_seq (String.to_seq s)))

let unhex s =
  if s = "-" then "" else
  String.init (String.length s / 2)
    (fun i -> Char.chr (int_of_string ("0x" ^ String.sub s (2 * i) 2)))

let b x = if x then "1" else "0"
let prefix r s p =
  if p > String.length s then "~" else
  match Rx.match_prefix r s p with Some n -> string_of_int n | None -> "~"

let compile f p =
  try Ok (f p) with
  | Rx.Parse_error -> Error "E:Parse_error"
  | Rx.Not_supported -> Error "E:Not_supported"
  (* explode (globx) runs off the end of a pattern ending in a backslash. *)
  | Invalid_argument _ -> Error "E:Invalid_argument"

(* A fixed LCG, so that every OCaml version generates the same corpus. *)
let state = ref 20261019
let random n =
  state := (!state * 1103515245 + 12345) land 0x7FFFFFFF;
  (!state lsr 8) mod n
let pick a = a.(random (Array.length a))
let random_string alphabet max_len =
  String.init (random (max_len + 1)) (fun _ -> pick alphabet)

let rx_inputs = [
  ""; "a"; "aa"; "aaa"; "aaaa"; "ab"; "abc"; "abab"; "b"; "ba"; "c"; "bcd";
  "abcd"; "x.o"; "."; "\n"; "a\nb"; "\na"; "a\n"; "]"; "-"; "{"; "|"; "*";
  "\\"; "("; "\xe0"; "/" ]

let rx_patterns = [
  ""; "a"; "abc"; "a*"; "a+"; "a?"; "a|b"; "(ab)*"; "a{2}"; "a{2,}"; "a{2,3}";
  "a{0}"; "a{0,1}"; "a{1,1}"; "a{,3}"; "a{3,2}"; "a{"; "a{2"; "a{2,3";
  "a{99999999999999999999}"; "[abc]"; "[^abc]"; "[a-c]"; "[c-a]"; "[]a]";
  "[a-]"; "[]-a]"; "[^]a]"; "[[.a.]]"; "[[.a]"; "[[.]"; "[[:alpha:]]";
  "[[=a=]]"; "[[]"; "."; ".*"; "\\."; "\\|"; "\\a"; "\\"; "*"; "a**"; "+a";
  "(a"; "a)"; "()"; "x|"; "|x"; "(|a)"; "["; "[a"; "\\{"; "{"; "}"; "a}";
  "ab|cd|ef"; "(a|ab)(c|bcd)"; "[.]"; ".*\\.o"; "\\("; "(a*)*"; "(a|b)*c";
  "a.c"; "[^\n]"; "a\nb"; "(a+)+b"; "((a))"; "a|"; "[a-a]"; "[\\]"; "]";
  "-"; ","; "a{1}{2}"; "(ab|a)(bc|c)?"; "[^]"; "[a-c-e]" ]

let anchored = [
  "^"; "$"; "^a"; "a$"; "^a$"; "^$"; "a^"; "$a"; "^a*$"; "a\n^b"; "a$\nb";
  "(^a|b)"; "(a$|b)"; "^(a|\nb)*$"; "a^b"; "a$b"; "x*^y"; "^^a"; "a$$" ]

(* Structured random patterns, always well-formed. *)
let rec random_rx depth =
  if depth = 0 || random 3 = 0 then
    pick [| "a"; "b"; "."; "[ab]"; "[^a]"; "\\."; "c"; "[a-b]" |]
  else
    match random 6 with
    | 0 -> random_rx (depth - 1) ^ random_rx (depth - 1)
    | 1 -> "(" ^ random_rx (depth - 1) ^ "|" ^ random_rx (depth - 1) ^ ")"
    | 2 -> "(" ^ random_rx (depth - 1) ^ ")*"
    | 3 -> "(" ^ random_rx (depth - 1) ^ ")+"
    | 4 -> "(" ^ random_rx (depth - 1) ^ ")?"
    | _ ->
        let i = random 3 in
        let j = if random 2 = 0 then "" else string_of_int (i + random 3) in
        "(" ^ random_rx (depth - 1) ^ ")" ^
        (match random 3 with
         | 0 -> Printf.sprintf "{%d}" i
         | _ -> Printf.sprintf "{%d,%s}" i j)

let raw_rx_alphabet =
  [| 'a'; 'b'; '.'; '*'; '+'; '?'; '|'; '('; ')'; '['; ']'; '-'; '{'; '}';
     '\\'; ','; '1'; '2' |]
let input_alphabet = [| 'a'; 'b'; 'c'; '.'; '\n'; '/' |]

let rx_lines oc pattern inputs =
  match compile Rx.rx pattern with
  | Error e -> Printf.fprintf oc "rx\t%s\t%s\n" (hex pattern) e
  | Ok r ->
      List.iter (fun s ->
          Printf.fprintf oc "rx\t%s\t%s\t%s\t%s\t%s\t%s\n" (hex pattern) (hex s)
            (b (Rx.match_string r s)) (b (Rx.match_substring r s))
            (prefix r s 0) (prefix r s 1)) inputs

let glob_inputs = [
  ""; "a"; "b"; "c"; ".a"; "a.o"; ".o"; "x/a"; "a/b"; "a/.b"; "ab"; "ba";
  "."; ".."; "*"; "a,b"; "{a,b}"; "bcd"; "ac"; "ad"; "bd"; "be"; "x"; "xy";
  "a/b/c"; "/a"; "a/"; ".a/b"; "x/.a"; "?"; "\\"; "abc.o"; "a.b.c" ]

let glob_patterns = [
  ""; "*"; "*.o"; "a*"; "*a"; "?"; "a?"; ".*"; ".?"; "[a-c]"; "[!a]"; "[^a]";
  "[.]a"; "a/b"; "*/b"; "a/*"; "a/.b"; "a/*b"; "\\*"; "\\"; "{a,b}";
  "{a,b}c{d,e}"; "x{,y}"; "{a,{b,c}}"; "{a"; "a}"; "a,b"; "["; "[a"; "a\\";
  "**"; "*.*"; "*?"; "?*"; "[.]*"; ".[a]"; "*/.*"; "[!.]*"; "a[]]"; "a[!]]";
  "*[a]"; "?[.]"; "\\{a,b}"; "{\\,,a}"; "a/{b,.c}"; "/a"; "a/"; "*/"; "[a-]";
  "*.{o,b}" ]

let glob_lines oc nodot x pattern =
  let f = if x then Rx.globx' nodot else Rx.glob' nodot in
  match compile f pattern with
  | Error e ->
      Printf.fprintf oc "glob\t%s\t%s\t%s\t%s\n" (b nodot) (b x) (hex pattern) e
  | Ok r ->
      List.iter (fun s ->
          Printf.fprintf oc "glob\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n" (b nodot) (b x)
            (hex pattern) (hex s) (b (Rx.match_string r s))
            (b (Rx.match_substring r s)) (prefix r s 0)) glob_inputs

let ci_patterns = [ "abc"; "[a-c]+"; "\xc0"; "[^A]"; "\xd7"; "[\xc0-\xde]"; "z" ]
let ci_inputs = [ "ABC"; "aBc"; "abc"; "\xe0"; "\xc0"; "a"; "A"; "\xf7"; "\xd7";
                  "\xfe"; "\xff"; "Z"; "z"; "B" ]

let header oc =
  Printf.fprintf oc
    "# Rx golden vectors, generated by tools/ocaml-fixtures/gen_rx.ml.\n\
     # Unison source: %s. OCaml %s, %d-bit.\n\
     # Regenerate with scripts/gen-ocaml-fixtures.sh; do not edit by hand.\n"
    (Option.value (Sys.getenv_opt "UNISON_COMMIT") ~default:"unknown")
    Sys.ocaml_version Sys.word_size

let () =
  match Array.to_list Sys.argv with
  | [_; "--anchors"] -> List.iter (fun p -> print_endline (hex p)) anchored
  | [_; "--anchor"; p] -> rx_lines stdout (unhex p) rx_inputs
  | [_; dir] ->
      let oc = open_out_bin (Filename.concat dir "rx.txt") in
      header oc;
      List.iter (fun p -> rx_lines oc p rx_inputs) rx_patterns;
      for _ = 1 to 300 do
        let p = random_rx 4 in
        rx_lines oc p (List.init 10 (fun _ -> random_string input_alphabet 7))
      done;
      for _ = 1 to 300 do
        let p = random_string raw_rx_alphabet 8 in
        rx_lines oc p (List.init 6 (fun _ -> random_string input_alphabet 5))
      done;
      List.iter (fun nodot ->
          List.iter (fun x -> List.iter (glob_lines oc nodot x) glob_patterns)
            [false; true]) [true; false];
      List.iter (fun p ->
          let r = Rx.case_insensitive (Rx.rx p) in
          List.iter (fun s ->
              Printf.fprintf oc "ci\t%s\t%s\t%s\n" (hex p) (hex s)
                (b (Rx.match_string r s))) ci_inputs) ci_patterns;
      close_out oc
  | _ -> prerr_endline "usage: gen_rx <dir> | --anchors | --anchor <hex>"; exit 2
