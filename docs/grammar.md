## type
```bnf
type := identifier
```
## funcion
```bnf
param := identifier ":" type
func := "fun" identifier "(" (param ("," param)*)? ")" ("->" type)? block
block := "{" statement* "}"
```