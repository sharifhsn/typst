#import "@preview/cetz:0.3.4"

#cetz.canvas({
  import cetz.draw: *
  line((0, 0), (1, 1), stroke: red + 1pt)
  line((1, 1), (2, 0), stroke: blue + 1pt)
})
