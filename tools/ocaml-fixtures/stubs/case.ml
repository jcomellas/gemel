(* Stand-in for Unison's src/case.ml, for building pred.ml in gen_pred.ml:
   the mode is set directly rather than from preferences. The methods
   below are copied from case.ml (only those pred.ml uses), with type
   annotations where case.ml's other methods fix the types:
   Derived from Unison, Copyright 1999-2020, Benjamin C. Pierce. *)

type mode = Sensitive | Insensitive | UnicodeSensitive | UnicodeInsensitive

let sensitiveOps = object
  method mode = Sensitive
  method normalizePattern (s : string) = s
  method caseInsensitiveMatch = false
  method normalizeMatchedString (s : string) = s
end

let insensitiveOps = object
  method mode = Insensitive
  method normalizePattern (s : string) = s
  method caseInsensitiveMatch = true
  method normalizeMatchedString (s : string) = s
end

let unicodeSensitiveOps = object
  method mode = UnicodeSensitive
  method normalizePattern p = Unicode.decompose p
  method caseInsensitiveMatch = false
  method normalizeMatchedString s = Unicode.decompose s
end

let unicodeInsensitiveOps = object
  method mode = UnicodeInsensitive
  method normalizePattern p = Unicode.normalize p
  method caseInsensitiveMatch = false
  method normalizeMatchedString s = Unicode.normalize s
end

let current = ref sensitiveOps
let () = ignore [!current; insensitiveOps; unicodeSensitiveOps; unicodeInsensitiveOps]

let ops () = !current
