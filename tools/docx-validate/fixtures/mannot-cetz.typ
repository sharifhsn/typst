#import "@preview/cetz:0.4.2"
#import "@preview/mannot:0.3.2": mark, annot-cetz

$
  (
    mark(0, tag: #<s1>),
    mark(2, tag: #<s2>),
    mark(4, tag: #<s3>),
    mark(3, tag: #<s4>),
    mark(5, tag: #<s5>),
    mark(1, tag: #<s6>)
  )

  #annot-cetz((<s1>, <s2>, <s3>, <s4>, <s5>, <s6>), cetz, {
    import cetz.draw: *
    set-style(
      mark: (end: "straight", length: 0.2em, width: 0.2em),
      stroke: red + 0.75pt,
    )
    bezier-through("s2.south", (rel: (x: -.2, y: -.15)), "s1.south")
    bezier-through("s3.south", (rel: (x: -.2, y: -.15)), "s2.south")
    bezier-through("s4.north", (rel: (x: -.3, y: .15)), "s2.north")
    bezier-through("s5.south", (rel: (x: -.2, y: -.15)), "s4.south")
    bezier-through("s6.north", (rel: (x: -.5, y: .25)), "s1.north")
  })
$
