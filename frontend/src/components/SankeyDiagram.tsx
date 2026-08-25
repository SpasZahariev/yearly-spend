import { useMemo, useState } from "react"

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
  onPin: (selection: Selection) => void
}

interface PositionedNode extends SankeyNodeData {
  x: number
  y: number
  height: number
  width: number
}

interface PositionedLink extends SankeyLinkData {
  x0: number
  x1: number
  y0: number
  y1: number
  width: number
}

const WIDTH = 960
const HEIGHT = 520
const NODE_WIDTH = 16
const NODE_GAP = 10
const TOP = 24
const BOTTOM = 24
const OTHER_COLOR = "#a1a1aa"

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
  const values = new Map<string, { incoming: number; outgoing: number }>()
  for (const link of links) {
    const source = values.get(link.source) ?? { incoming: 0, outgoing: 0 }
    source.outgoing += link.value
    values.set(link.source, source)
    const target = values.get(link.target) ?? { incoming: 0, outgoing: 0 }
    target.incoming += link.value
    values.set(link.target, target)
  }

  return {
    nodes: nodes.map((node) => {
      const value = values.get(node.id)
      return {
        ...node,
        value: value === undefined ? node.value : Math.max(value.incoming, value.outgoing),
      }
    }),
    links,
    visibleCategories,
  }
}

function layoutSankey(data: SankeyData) {
  const normalised = normaliseData(data)
  const nodesByColumn = [0, 1, 2].map((column) =>
    normalised.nodes
      .filter((node) => node.column === column)
      .sort((a, b) => b.value - a.value || a.label.localeCompare(b.label)),
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

  const outgoing = new Map<string, number>()
  const incoming = new Map<string, number>()
  const links: PositionedLink[] = [...normalised.links]
    .sort((a, b) => {
      const aTarget = nodes.get(a.target)
      const bTarget = nodes.get(b.target)
      return (
        Number(b.kind === "transfer") - Number(a.kind === "transfer") ||
        (aTarget?.y ?? 0) - (bTarget?.y ?? 0)
      )
    })
    .flatMap((link) => {
      const source = nodes.get(link.source)
      const target = nodes.get(link.target)
      if (source === undefined || target === undefined) return []

      const width = Math.max(1.5, link.value * scale)
      const y0 = source.y + (outgoing.get(source.id) ?? 0)
      const y1 = target.y + (incoming.get(target.id) ?? 0)
      outgoing.set(source.id, (outgoing.get(source.id) ?? 0) + width)
      incoming.set(target.id, (incoming.get(target.id) ?? 0) + width)
      return [
        {
          ...link,
          x0: source.x + source.width,
          x1: target.x,
          y0,
          y1,
          width,
        },
      ]
    })

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

export function SankeyDiagram({ data, format, onPin }: SankeyDiagramProps) {
  const [hovered, setHovered] = useState<string | null>(null)
  const layout = useMemo(() => layoutSankey(data), [data])

  if (layout.links.length === 0) {
    return (
      <div className="flex h-[28rem] items-center justify-center text-sm text-muted-foreground">
        no account flows recorded
      </div>
    )
  }

  const nodeById = new Map(layout.nodes.map((node) => [node.id, node]))
  const accountNodes = layout.nodes.filter((node) => node.column < 2)
  const categoryNodes = layout.nodes.filter((node) => node.column === 2)
  const transferLinks = layout.links.filter((link) => link.kind === "transfer")

  function pinNode(node: PositionedNode) {
    onPin({
      chart: "sankey",
      series: node.column === 2 ? "category" : "account",
      label: node.label,
      value: node.value,
      year: data.year,
      month: data.month ?? undefined,
      category: node.column === 2 ? node.label : undefined,
    })
  }

  function pinLink(link: PositionedLink) {
    const source = nodeById.get(link.source)
    const target = nodeById.get(link.target)
    if (source === undefined || target === undefined) return
    onPin({
      chart: "sankey",
      series: link.kind,
      label: `${source.label} -> ${target.label}`,
      value: link.value,
      year: data.year,
      month: data.month ?? undefined,
      category: link.kind === "spend" ? target.label : undefined,
    })
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
              const isActive =
                hovered === null || hovered === link.source || hovered === link.target
              return (
                <path
                  key={`${link.source}:${link.target}:${link.kind}`}
                  d={ribbonPath(link)}
                  fill={link.color}
                  fillOpacity={
                    link.kind === "transfer" ? (isActive ? 0.42 : 0.1) : isActive ? 0.34 : 0.08
                  }
                  className="cursor-pointer transition-opacity"
                  role="button"
                  tabIndex={0}
                  aria-label={`${link.kind}: ${nodeById.get(link.source)?.label ?? link.source} to ${nodeById.get(link.target)?.label ?? link.target}, ${format(link.value)}`}
                  onMouseEnter={() => setHovered(link.source)}
                  onMouseLeave={() => setHovered(null)}
                  onFocus={() => setHovered(link.source)}
                  onBlur={() => setHovered(null)}
                  onClick={() => pinLink(link)}
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
              const isActive = hovered === null || hovered === node.id
              const label = labelPosition(node)
              return (
                <g
                  key={node.id}
                  className="cursor-pointer"
                  role="button"
                  tabIndex={0}
                  onMouseEnter={() => setHovered(node.id)}
                  onMouseLeave={() => setHovered(null)}
                  onFocus={() => setHovered(node.id)}
                  onBlur={() => setHovered(null)}
                  onClick={() => pinNode(node)}
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
                  {node.height < 22 ? (
                    <text
                      x={label.x}
                      y={node.y + node.height / 2 + 4}
                      textAnchor={label.anchor}
                      fill="#111111"
                      fontSize="12"
                      fontWeight="600"
                    >
                      {node.label} · {format(node.value)}
                    </text>
                  ) : (
                    <>
                      <text
                        x={label.x}
                        y={node.y + Math.min(node.height / 2, 10)}
                        textAnchor={label.anchor}
                        fill="#111111"
                        fontSize="14"
                        fontWeight="600"
                      >
                        {node.label}
                      </text>
                      <text
                        x={label.x}
                        y={node.y + Math.min(node.height / 2, 10) + 17}
                        textAnchor={label.anchor}
                        fill="#6d6d6d"
                        fontSize="12"
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
        <span className="inline-flex items-center gap-2">
          <span className="h-2.5 w-8 bg-slate-400/70" aria-hidden="true" />
          paired account transfers
        </span>
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
