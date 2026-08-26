import { useMemo, useState, type MouseEvent } from "react"

import { anchorFromClick } from "@/lib/utils"
import type { Selection } from "@/types"

export interface SankeyNodeData {
  id: string
  label: string
  color: string
  column: number
  value: number
}

export interface SankeyLinkData {
  source: string
  target: string
  value: number
  color: string
  kind: "spend" | "transfer"
}

export interface SankeyData {
  year: number
  month: number | null
  nodes: SankeyNodeData[]
  links: SankeyLinkData[]
}

interface SankeyDiagramProps {
  data: SankeyData
  format: (value: number) => string
  onPin: (selection: Selection, anchor: { x: number; y: number }) => void
}

interface HoverState {
  linkId: string | null
  nodeIds: string[]
}

interface NodeTotals {
  incoming: number
  outgoing: number
}

interface FlowNodeData extends SankeyNodeData {
  totals: NodeTotals
}

interface PositionedNode extends SankeyNodeData {
  x: number
  y: number
  height: number
  width: number
}

interface PositionedLink extends SankeyLinkData {
  id: string
  x0: number
  x1: number
  y0: number
  y1: number
  width: number
}

const WIDTH = 960
const HEIGHT = 1080
const NODE_WIDTH = 16
const NODE_GAP = 22
const TOP = 32
const BOTTOM = 32
const OTHER_COLOR = "#a1a1aa"

function sortMetric(node: FlowNodeData) {
  return node.column === 2 ? node.totals.incoming : node.totals.outgoing
}

function pushMapList<T>(map: Map<string, T[]>, key: string, value: T) {
  const list = map.get(key)
  if (list === undefined) {
    map.set(key, [value])
    return
  }
  list.push(value)
}

function normaliseData(data: SankeyData) {
  const categoryNodes = data.nodes
    .filter((node) => node.column === 2)
    .sort((a, b) => b.value - a.value)
  const visibleCategories = categoryNodes.slice(0, 10)
  const hiddenCategoryIds = new Set(categoryNodes.slice(10).map((node) => node.id))
  const hiddenCategoryValue = categoryNodes.slice(10).reduce((total, node) => total + node.value, 0)

  const nodes = data.nodes
    .filter((node) => !hiddenCategoryIds.has(node.id))
    .map((node) => ({ ...node }))

  if (hiddenCategoryValue > 0) {
    nodes.push({
      id: "category:other",
      label: "other",
      color: OTHER_COLOR,
      column: 2,
      value: hiddenCategoryValue,
    })
  }

  const links = data.links.map((link) =>
    hiddenCategoryIds.has(link.target)
      ? { ...link, target: "category:other", color: OTHER_COLOR }
      : { ...link },
  )
  const totals = new Map<string, NodeTotals>()
  for (const link of links) {
    const source = totals.get(link.source) ?? { incoming: 0, outgoing: 0 }
    source.outgoing += link.value
    totals.set(link.source, source)
    const target = totals.get(link.target) ?? { incoming: 0, outgoing: 0 }
    target.incoming += link.value
    totals.set(link.target, target)
  }

  return {
    nodes: nodes.map((node) => {
      const value = totals.get(node.id) ?? { incoming: 0, outgoing: 0 }
      return {
        ...node,
        totals: value,
        value: Math.max(value.incoming, value.outgoing),
      }
    }),
    links,
    totals,
    visibleCategories,
  }
}

function layoutSankey(data: SankeyData) {
  const normalised = normaliseData(data)
  const nodesByColumn = [0, 1, 2].map((column) =>
    normalised.nodes
      .filter((node) => node.column === column)
      .sort(
        (a, b) =>
          sortMetric(b) - sortMetric(a) || b.value - a.value || a.label.localeCompare(b.label),
      ),
  )
  const maxTotal = Math.max(
    ...nodesByColumn.map((column) => column.reduce((total, node) => total + node.value, 0)),
    0,
  )
  const maxCount = Math.max(...nodesByColumn.map((column) => column.length), 1)
  const usableHeight = HEIGHT - TOP - BOTTOM - NODE_GAP * (maxCount - 1)
  const scale = maxTotal > 0 ? usableHeight / maxTotal : 1
  const xByColumn = [28, 472, WIDTH - 28 - NODE_WIDTH]
  const nodes = new Map<string, PositionedNode>()

  for (const [columnIndex, column] of nodesByColumn.entries()) {
    const heights = column.map((node) => Math.max(8, node.value * scale))
    const totalHeight = heights.reduce((total, height) => total + height, 0)
    const gap = NODE_GAP
    let y = TOP + Math.max(0, (HEIGHT - TOP - BOTTOM - totalHeight - gap * (column.length - 1)) / 2)

    column.forEach((node, index) => {
      const height = heights[index]
      nodes.set(node.id, {
        ...node,
        x: xByColumn[columnIndex],
        y,
        height,
        width: NODE_WIDTH,
      })
      y += height + gap
    })
  }

  const baseLinks: PositionedLink[] = normalised.links.flatMap((link, index) => {
    const source = nodes.get(link.source)
    const target = nodes.get(link.target)
    if (source === undefined || target === undefined) return []
    return [
      {
        ...link,
        id: `${link.source}:${link.target}:${link.kind}:${index}`,
        x0: source.x + source.width,
        x1: target.x,
        y0: 0,
        y1: 0,
        width: Math.max(1.5, link.value * scale),
      },
    ]
  })

  const outgoingBySource = new Map<string, PositionedLink[]>()
  const incomingByTarget = new Map<string, PositionedLink[]>()
  for (const link of baseLinks) {
    pushMapList(outgoingBySource, link.source, link)
    pushMapList(incomingByTarget, link.target, link)
  }

  for (const [sourceId, links] of outgoingBySource) {
    const source = nodes.get(sourceId)
    if (source === undefined) continue
    links.sort(
      (a, b) =>
        b.value - a.value ||
        (nodes.get(a.target)?.y ?? 0) - (nodes.get(b.target)?.y ?? 0) ||
        a.target.localeCompare(b.target) ||
        a.id.localeCompare(b.id),
    )
    let offset = 0
    for (const link of links) {
      link.y0 = source.y + offset
      offset += link.width
    }
  }

  for (const [targetId, links] of incomingByTarget) {
    const target = nodes.get(targetId)
    if (target === undefined) continue
    links.sort(
      (a, b) =>
        b.value - a.value ||
        (nodes.get(a.source)?.y ?? 0) - (nodes.get(b.source)?.y ?? 0) ||
        a.source.localeCompare(b.source) ||
        a.id.localeCompare(b.id),
    )
    let offset = 0
    for (const link of links) {
      link.y1 = target.y + offset
      offset += link.width
    }
  }

  const links = baseLinks.sort(
    (a, b) => a.x0 - b.x0 || a.y0 - b.y0 || b.width - a.width || a.id.localeCompare(b.id),
  )

  return { nodes: [...nodes.values()], links, visibleCategories: normalised.visibleCategories }
}

function ribbonPath(link: PositionedLink) {
  const curve = Math.max(36, (link.x1 - link.x0) * 0.42)
  return [
    `M ${link.x0} ${link.y0}`,
    `C ${link.x0 + curve} ${link.y0}, ${link.x1 - curve} ${link.y1}, ${link.x1} ${link.y1}`,
    `L ${link.x1} ${link.y1 + link.width}`,
    `C ${link.x1 - curve} ${link.y1 + link.width}, ${link.x0 + curve} ${link.y0 + link.width}, ${link.x0} ${link.y0 + link.width}`,
    "Z",
  ].join(" ")
}

function labelPosition(node: PositionedNode) {
  if (node.column === 2) {
    return { x: node.x - 10, anchor: "end" as const }
  }
  return { x: node.x + node.width + 10, anchor: "start" as const }
}

function labelSpaceBelow(node: PositionedNode, nextY: number | undefined) {
  // The last node in a column has no label below it, so it can always stack
  // its amount line under the name (the bottom margin absorbs the overflow).
  if (nextY === undefined) return Number.POSITIVE_INFINITY
  return nextY - node.y
}

export function SankeyDiagram({ data, format, onPin }: SankeyDiagramProps) {
  const [hovered, setHovered] = useState<HoverState | null>(null)
  const layout = useMemo(() => layoutSankey(data), [data])

  if (layout.links.length === 0) {
    return (
      <div className="flex aspect-[8/9] w-full items-center justify-center text-sm text-muted-foreground">
        no account flows recorded
      </div>
    )
  }

  const nodeById = new Map(layout.nodes.map((node) => [node.id, node]))
  const nextNodeYById = new Map<string, number>()
  for (const column of [0, 1, 2]) {
    const nodes = layout.nodes.filter((node) => node.column === column).sort((a, b) => a.y - b.y)
    for (let index = 0; index < nodes.length - 1; index += 1) {
      nextNodeYById.set(nodes[index].id, nodes[index + 1].y)
    }
  }
  const accountNodes = layout.nodes.filter((node) => node.column < 2)
  const categoryNodes = layout.nodes.filter((node) => node.column === 2)
  const transferLinks = layout.links.filter((link) => link.kind === "transfer")
  const transferLegendItems = [
    ...new Map(
      transferLinks.map((link) => [
        link.target,
        {
          id: link.target,
          label: nodeById.get(link.target)?.label ?? link.target,
          color: link.color,
        },
      ]),
    ).values(),
  ]

  function isNodeHighlighted(nodeId: string) {
    return hovered?.nodeIds.includes(nodeId) ?? false
  }

  function isNodeActive(nodeId: string) {
    return hovered === null || hovered.nodeIds.includes(nodeId)
  }

  function isLinkActive(link: PositionedLink) {
    if (hovered === null) return true
    if (hovered.linkId !== null) return hovered.linkId === link.id
    return hovered.nodeIds.includes(link.source) || hovered.nodeIds.includes(link.target)
  }

  function pinNode(node: PositionedNode, event: MouseEvent<SVGGElement>) {
    onPin(
      {
        chart: "sankey",
        series: node.column === 2 ? "category" : "account",
        label: node.label,
        value: node.value,
        year: data.year,
        month: data.month ?? undefined,
        category: node.column === 2 ? node.label : undefined,
      },
      anchorFromClick(event),
    )
  }

  function hoverNode(nodeId: string) {
    setHovered({ linkId: null, nodeIds: [nodeId] })
  }

  function hoverLink(link: PositionedLink) {
    setHovered({ linkId: link.id, nodeIds: [link.source, link.target] })
  }

  function pinLink(link: PositionedLink, event: MouseEvent<SVGPathElement>) {
    const source = nodeById.get(link.source)
    const target = nodeById.get(link.target)
    if (source === undefined || target === undefined) return
    onPin(
      {
        chart: "sankey",
        series: link.kind,
        label: `${source.label} -> ${target.label}`,
        value: link.value,
        year: data.year,
        month: data.month ?? undefined,
        category: link.kind === "spend" ? target.label : undefined,
      },
      anchorFromClick(event),
    )
  }

  return (
    <div className="flex flex-col gap-3">
      <div className="overflow-x-auto">
        <svg
          viewBox={`0 0 ${WIDTH} ${HEIGHT}`}
          className="min-w-[720px] w-full"
          role="img"
          aria-label={`Account spending and transfer flows for ${data.year}`}
        >
          <g>
            {layout.links.map((link) => {
              const isActive = isLinkActive(link)
              return (
                <path
                  key={link.id}
                  d={ribbonPath(link)}
                  fill={link.color}
                  fillOpacity={
                    link.kind === "transfer" ? (isActive ? 0.42 : 0.1) : isActive ? 0.34 : 0.08
                  }
                  className="cursor-pointer transition-opacity"
                  role="button"
                  tabIndex={0}
                  aria-label={`${link.kind}: ${nodeById.get(link.source)?.label ?? link.source} to ${nodeById.get(link.target)?.label ?? link.target}, ${format(link.value)}`}
                  onMouseEnter={() => hoverLink(link)}
                  onMouseLeave={() => setHovered(null)}
                  onFocus={() => hoverLink(link)}
                  onBlur={() => setHovered(null)}
                  onClick={(event) => pinLink(link, event)}
                >
                  <title>
                    {link.kind}: {nodeById.get(link.source)?.label} to{" "}
                    {nodeById.get(link.target)?.label} · {format(link.value)}
                  </title>
                </path>
              )
            })}
          </g>

          <g>
            {layout.nodes.map((node) => {
              const isActive = isNodeActive(node.id)
              const isHighlighted = isNodeHighlighted(node.id)
              const label = labelPosition(node)
              const showStackedLabel =
                node.column === 2
                  ? labelSpaceBelow(node, nextNodeYById.get(node.id)) >= 26
                  : node.height >= 22
              return (
                <g
                  key={node.id}
                  className="cursor-pointer"
                  role="button"
                  tabIndex={0}
                  onMouseEnter={() => hoverNode(node.id)}
                  onMouseLeave={() => setHovered(null)}
                  onFocus={() => hoverNode(node.id)}
                  onBlur={() => setHovered(null)}
                  onClick={(event) => pinNode(node, event)}
                  aria-label={`${node.label}, ${format(node.value)}`}
                >
                  <rect
                    x={node.x}
                    y={node.y}
                    width={node.width}
                    height={node.height}
                    fill={node.color}
                    fillOpacity={isActive ? 1 : 0.35}
                  />
                  {!showStackedLabel ? (
                    <text
                      x={label.x}
                      y={node.y + node.height / 2 + 4}
                      textAnchor={label.anchor}
                      fill="#111111"
                      fontSize="12"
                      fontWeight={isHighlighted ? "700" : "600"}
                      textDecoration={isHighlighted ? "underline" : "none"}
                    >
                      {node.label} · {format(node.value)}
                    </text>
                  ) : (
                    <>
                      <text
                        x={label.x}
                        y={node.column === 2 ? node.y + 12 : node.y + Math.min(node.height / 2, 10)}
                        textAnchor={label.anchor}
                        fill="#111111"
                        fontSize="14"
                        fontWeight={isHighlighted ? "700" : "600"}
                        textDecoration={isHighlighted ? "underline" : "none"}
                      >
                        {node.label}
                      </text>
                      <text
                        x={label.x}
                        y={
                          node.column === 2
                            ? node.y + 28
                            : node.y + Math.min(node.height / 2, 10) + 17
                        }
                        textAnchor={label.anchor}
                        fill="#6d6d6d"
                        fontSize="12"
                        fontWeight={isHighlighted ? "600" : "400"}
                      >
                        {format(node.value)}
                      </text>
                    </>
                  )}
                </g>
              )
            })}
          </g>
        </svg>
      </div>

      <div className="flex flex-wrap items-center gap-x-5 gap-y-2 border-t border-border pt-3 text-xs text-muted-foreground">
        <span className="inline-flex items-center gap-2">
          <span className="h-2.5 w-8 bg-brand-pink/70" aria-hidden="true" />
          category spend
        </span>
        {transferLegendItems.length > 0 && (
          <span className="inline-flex flex-wrap items-center gap-x-3 gap-y-1">
            <span>paired account transfers</span>
            {transferLegendItems.map((item) => (
              <span key={item.id} className="inline-flex items-center gap-2">
                <span
                  className="h-2.5 w-8"
                  aria-hidden="true"
                  style={{ backgroundColor: item.color }}
                />
                to {item.label}
              </span>
            ))}
          </span>
        )}
        {categoryNodes.length > 10 && (
          <span>top {Math.min(10, layout.visibleCategories.length)} categories + other</span>
        )}
        {accountNodes.length > 0 && transferLinks.length === 0 && (
          <span>no paired transfers in this period</span>
        )}
      </div>
    </div>
  )
}
