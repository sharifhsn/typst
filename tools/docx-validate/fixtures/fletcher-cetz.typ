#import "@preview/fletcher:0.5.8": diagram, edge, node

#diagram(
  node-stroke: 1pt,
  edge-stroke: 1pt,
  spacing: (0.8em, 0.4em),
  node-inset: 0.4em,
  edge-corner-radius: 8pt,
  node((0, 0), width: 1.25em, shape: "circle", $a_i$),
  node((1, -1), width: 1.25em, shape: "circle", $s_1$),
  node((2, -1), width: 1.25em, shape: "circle", $s_2$),
  node((3, -1), width: 1.25em, shape: "circle", $s_3$),
  edge((0, 0), "r,u"),
  edge((0, 0), "r,r,u"),
  edge((0, 0), "r,r,r,u"),
)
