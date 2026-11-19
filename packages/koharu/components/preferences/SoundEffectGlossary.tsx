'use client'

import { useQuery } from '@tanstack/react-query'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'

import { call } from '@/lib/backend'
import { glossaryKey } from '@/lib/queries'
import { commands } from '@koharu/ipc/protocol'
import { Button } from '@koharu/ui/components/button'
import { Input } from '@koharu/ui/components/input'
import { ScrollArea } from '@koharu/ui/components/scroll-area'

export function SoundEffectGlossary() {
  const { t } = useTranslation()
  const [search, setSearch] = useState('')
  const [editing, setEditing] = useState<{
    pair: string
    source: string
    previous: string
    value: string
  } | null>(null)
  const [busy, setBusy] = useState(false)
  const query = useQuery({
    queryKey: [...glossaryKey, 'global-sfx'],
    queryFn: () => call(commands.getSfxGlossary),
  })
  const rows = Object.entries(query.data?.languages ?? {}).flatMap(([pair, entries]) =>
    Object.entries(entries)
      .filter(([source, value]) => source.includes(search) || value.includes(search))
      .map(([source, value]) => ({ pair, source, value })),
  )
  const save = async (value: string | null) => {
    if (!editing || busy) return
    setBusy(true)
    try {
      await call(commands.editSfxTerm, editing.pair, editing.source, editing.previous, value)
      setEditing(null)
    } finally {
      await query.refetch()
      setBusy(false)
    }
  }
  return (
    <section className='space-y-2'>
      <h2 className='text-base font-semibold'>{t('glossary.sfxGlossary')}</h2>
      <Input
        value={search}
        onChange={(e) => setSearch(e.target.value)}
        placeholder={t('glossary.search')}
        aria-label={t('glossary.search')}
      />
      <ScrollArea className='max-h-96'>
        {query.isSuccess && !rows.length && (
          <p className='p-2 text-sm text-muted-foreground'>{t('glossary.noEntries')}</p>
        )}
        {rows.map(({ pair, source, value }) => (
          <div
            key={JSON.stringify([pair, source])}
            className='flex flex-wrap items-center gap-2 border-b py-2 text-sm'
          >
            <span className='text-xs text-muted-foreground'>{pair}</span>
            <strong className='wrap-anywhere'>{source}</strong>
            {editing?.pair === pair && editing.source === source ? (
              <>
                <Input
                  className='min-w-24 flex-1'
                  aria-label={t('glossary.translation')}
                  value={editing.value}
                  onChange={(e) => setEditing({ ...editing, value: e.target.value })}
                />
                <Button
                  size='xs'
                  disabled={busy || !editing.value.trim()}
                  onClick={() => void save(editing.value).catch(() => undefined)}
                >
                  {t('glossary.save')}
                </Button>
                <Button
                  size='xs'
                  variant='outline'
                  disabled={busy}
                  onClick={() => void save(null).catch(() => undefined)}
                >
                  {t('glossary.remove')}
                </Button>
                <Button size='xs' variant='ghost' disabled={busy} onClick={() => setEditing(null)}>
                  {t('glossary.cancel')}
                </Button>
              </>
            ) : (
              <>
                <span className='flex-1 wrap-anywhere'>{value}</span>
                <Button
                  size='xs'
                  variant='outline'
                  disabled={busy}
                  onClick={() => setEditing({ pair, source, previous: value, value })}
                >
                  {t('glossary.edit')}
                </Button>
              </>
            )}
          </div>
        ))}
      </ScrollArea>
    </section>
  )
}
