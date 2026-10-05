; norm: `plainDollars` escapes every dollar sign so the grammar's LaTeX
; extension cannot pair two prices into a math span. Show the escape as the
; plain dollar the model wrote.
((backslash_escape) @conceal
  (#eq? @conceal "\\$")
  (#set! conceal "$"))
