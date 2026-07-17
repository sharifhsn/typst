#import "@preview/fletcher:0.5.8": diagram, edge, node

// Self-contained reduction of the tree helper used by
// hemisemidemipresent-book. Keep it here rather than importing a sibling corpus
// checkout so the regression fixture survives on a clean machine.
#let parent(row, value, index) = {
  let results = ()
  for (i, v) in row.enumerate() {
    if v == value - 1 and i < index {
      results.push((i, v))
    }
  }
  results.last()
}

#let pss(toprow, bottomrow, ..args) = {
  let node-width = 1.25em
  let nodes = (
    node((-1, 1), `+`, width: node-width, shape: "circle", name: <root>),
  )
  let edges = ()
  for (i, height) in toprow.enumerate() {
    let value = bottomrow.at(i)
    nodes.push(node(
      (i, -height),
      width: node-width,
      shape: "circle",
      text[#value],
      name: label("circle" + str(i)),
    ))
    if height == 0 {
      edges.push(edge((-1, 1), (("r",) * (i + 1) + ("u",)).join(",")))
    } else {
      let (parent-index, _) = parent(toprow, height, i)
      edges.push(edge(
        (parent-index, -height + 1),
        (("r",) * (i - parent-index) + ("u",)).join(","),
      ))
    }
  }
  diagram(
    node-stroke: 1pt,
    edge-stroke: 1pt,
    spacing: (0.8em, 0.4em),
    node-inset: 0.4em,
    edge-corner-radius: 8pt,
    ..nodes,
    ..edges,
    ..args,
  )
}

#let prss(row, ..args) = pss(row, row, ..args)

#prss((0, 1, 2, 3, 2, 1))
#figure(prss((0, 1, 2, 3, 2, 1)))

#let smoltext(content) = [
  #set text(size: 8pt)
  #content
]

#table(
  columns: 2,
  [first], smoltext(pss((0, 1, 0, 1), (0, 1, 0, 1))),
  [second], smoltext(pss((0, 1, 1, 2, 2, 3), (0, 1, 0, 1, 0, 1))),
)

#let smolprss(seq) = [
  #set text(size: 8pt)
  #prss(seq)
]

#table(
  columns: 4,
  [sequence], [tree], [notation], [ordinal],
  [empty], smolprss(()), [zero], [0],
  [(0)], smolprss((0,)), [one], [1],
  [(0,1)], smolprss((0, 1)), [omega], [omega],
  [(0,1,0)], smolprss((0, 1, 0)), [omega plus one], [omega plus one],
  [(0,1,0,1)], smolprss((0, 1, 0, 1)), [omega times two], [omega times two],
  [(0,1,1)], smolprss((0, 1, 1)), [omega squared], [omega squared],
  [(0,1,2,3,2,1)], smolprss((0, 1, 2, 3, 2, 1)), [larger], [larger],
)
