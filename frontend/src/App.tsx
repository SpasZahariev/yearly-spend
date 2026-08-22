import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card"

export default function App() {
  return (
    <div className="flex min-h-dvh flex-col bg-background text-foreground">
      <header className="pixel-rows border-b-2 border-border px-6 py-4">
        <div className="mx-auto flex w-full max-w-5xl items-baseline justify-between">
          <h1 className="font-pixel text-2xl tracking-tight">
            yearly-spend<span className="animate-step-blink">_</span>
          </h1>
          <Badge variant="outline">local</Badge>
        </div>
      </header>

      <div className="dither-band" aria-hidden="true" />

      <main className="flex flex-1 items-center justify-center px-6 py-16">
        <Card className="w-full max-w-lg animate-step-in">
          <CardHeader>
            <CardTitle>nothing ingested yet</CardTitle>
            <CardDescription>
              Statements under <code className="font-mono text-xs">statements/</code> are parsed by
              the <code className="font-mono text-xs">spend</code> CLI into DuckDB. The dashboard
              fills in once data exists.
            </CardDescription>
          </CardHeader>
          <CardContent>
            <Button variant="outline" className="font-mono text-xs">
              spend ingest statements/
            </Button>
          </CardContent>
        </Card>
      </main>

      <footer className="border-t-2 border-border px-6 py-3">
        <div className="mx-auto flex w-full max-w-5xl items-center justify-between text-xs text-muted-foreground">
          <span className="font-mono">duckdb · axum · react</span>
          <span>local only</span>
        </div>
      </footer>
    </div>
  )
}
