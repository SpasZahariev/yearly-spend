import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card"

export function EmptyState() {
  return (
    <main className="flex min-w-0 flex-1 items-center justify-center px-4 py-16 sm:px-6 lg:overflow-y-auto">
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
          <code className="font-mono text-xs">spend ingest statements/</code>
        </CardContent>
      </Card>
    </main>
  )
}
