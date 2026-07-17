#set page(paper: "a4", margin: (x: 25mm, y: 22mm))
#set text(font: "Libertinus Serif", size: 12pt)
#set par(leading: 0.7em, spacing: 1.35em, justify: true)
#set math.equation(numbering: "(1)")
#let native-math = sys.inputs.at("native-math", default: "true") == "true"

#for i in range(80) [
  #if native-math [
    Definition #i: Let $R$ be a ring and $p$ a prime ideal. This prose contains
    inline mathematics but otherwise remains deliberately uniform across every
    repeated sample.
  ] else [
    Definition #i: Let R be a ring and p a prime ideal. This prose contains no
    mathematics and otherwise remains deliberately uniform across every sample.
  ]

  #if native-math {
    $ dim R = sup {n | p_0 supset p_1 supset dots supset p_n} $
  } else {
    align(center)[dim R equals the supremum of a descending chain]
  }

  The following sentence closes the sample and gives the display equation two
  ordinary paragraph boundaries.
]
