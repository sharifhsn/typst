#let f(i) = { let s = 0; for j in range(300) { s += calc.rem(i*j + j, 7) }; s }
#let r = range(1000)
Total: #(r.map(f).sum())
