'use client'

import { useQuery } from '@tanstack/react-query'
import { BookOpenText, Trash2, X } from 'lucide-react'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'

import { GlossaryManager } from '@/components/editor/GlossaryManager'
import { showEvidence } from '@/components/editor/ReviewPanel'
import { call, dispatch } from '@/lib/backend'
import { glossaryKey, refresh } from '@/lib/queries'
import { useKoharuStore } from '@/lib/store'
import {
  commands,
  type GlossaryEntry,
  type LayerId,
  type PageId,
  type ProjectId,
} from '@koharu/ipc/protocol'
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
  AlertDialogTrigger,
} from '@koharu/ui/components/alert-dialog'
import { Button } from '@koharu/ui/components/button'
import {
  Dialog,
  DialogClose,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from '@koharu/ui/components/dialog'
import { ScrollArea } from '@koharu/ui/components/scroll-area'

export function GlossaryPanel({ project, active }: { project: ProjectId; active: boolean }) {
  const { t } = useTranslation()
  const { data } = useQuery({
    queryKey: [...glossaryKey, project],
    queryFn: () => call(commands.getGlossary),
    enabled: active,
  })
  const model = useKoharuStore((state) => {
    const pipeline = state.preferences?.pipeline
    return pipeline?.glossary_analysis?.model || pipeline?.translator?.model
  })
  const pipelineBusy = useKoharuStore((state) =>
    Object.values(state.jobs).some((job) => job.state === 'running'),
  )
  const analysis = useKoharuStore((state) =>
    Object.values(state.jobs).find(
      (job) => job.state === 'running' && (job.glossary || job.stage === 'glossary'),
    ),
  )
  const [managerOpen, setManagerOpen] = useState(false)
  const [clearOpen, setClearOpen] = useState(false)
  const [busy, setBusy] = useState(false)
  const [action, setAction] = useState('')
  const [checked, setChecked] = useState(false)
  const list = data?.entries ?? []
  const candidates = data?.candidates
  const safe = data?.project === project
  const canClear =
    safe &&
    !busy &&
    !pipelineBusy &&
    (list.length > 0 || !!candidates?.length || data.suggestions.length > 0)
  const globalCount = useKoharuStore((state) => state.preferences?.pipeline.glossary?.length ?? 0)
  const blockedCount = list.filter(
    (item) => item.kind === 'preserve' || item.kind === 'preserve_block',
  ).length

  const openLine = (page: PageId, layer: LayerId) => {
    void showEvidence({ page, layer }).catch(() => undefined)
  }

  const save = async (next: GlossaryEntry[]) => {
    if (!safe) return
    setBusy(true)
    try {
      await call(commands.saveGlossary, project, list, next)
      await refresh(glossaryKey)
    } finally {
      setBusy(false)
    }
  }

  const extract = async () => {
    setBusy(true)
    try {
      const runs = await call(commands.extractSource, project)
      setAction(runs.length ? t('glossary.extractionStarted') : t('glossary.extractionCurrent'))
    } finally {
      setBusy(false)
    }
  }

  const clearGlossary = async () => {
    if (!canClear) return
    setBusy(true)
    try {
      await call(commands.edit, {
        label: 'edit_glossary',
        ops: [{ op: 'clear_glossary', project }],
      })
      setClearOpen(false)
      await refresh(glossaryKey)
    } finally {
      setBusy(false)
    }
  }

  const analyze = async () => {
    if (!model) return
    setBusy(true)
    setAction('')
    try {
      await call(commands.analyzeGlossary, project)
    } finally {
      setBusy(false)
    }
  }

  return (
    <ScrollArea className='h-full min-h-0 flex-1'>
      <div className='space-y-4 p-2.5 text-xs'>
        <div className='flex flex-wrap gap-1.5'>
          <Button
            size='sm'
            variant='outline'
            disabled={!safe || busy || pipelineBusy}
            onClick={() => void extract().catch(() => undefined)}
          >
            {t('glossary.extract')}
          </Button>
          <Button
            size='sm'
            variant='outline'
            disabled={!data?.lines}
            onClick={() => dispatch(commands.exportSource)}
          >
            {t('glossary.exportSource')}
          </Button>
          <Button
            size='sm'
            variant='outline'
            disabled={!safe || !data.lines || busy || pipelineBusy || !model}
            onClick={() => void analyze().catch(() => undefined)}
          >
            {t('glossary.analyze')}
          </Button>
          <Button
            size='sm'
            variant='outline'
            disabled={!list.length && !globalCount}
            onClick={() => setChecked((visible) => !visible)}
          >
            {t('glossary.check')}
          </Button>
          <AlertDialog open={clearOpen} onOpenChange={setClearOpen}>
            <AlertDialogTrigger
              render={<Button size='sm' variant='outline' disabled={!canClear} />}
            >
              <Trash2 /> {t('glossary.clearProject')}
            </AlertDialogTrigger>
            <AlertDialogContent>
              <AlertDialogHeader>
                <AlertDialogTitle>{t('glossary.clearProject')}</AlertDialogTitle>
                <AlertDialogDescription>{t('glossary.clearDescription')}</AlertDialogDescription>
              </AlertDialogHeader>
              <AlertDialogFooter>
                <AlertDialogCancel>{t('glossary.cancel')}</AlertDialogCancel>
                <AlertDialogAction
                  variant='destructive'
                  disabled={!canClear}
                  onClick={() => void clearGlossary().catch(() => undefined)}
                >
                  {t('glossary.clearProject')}
                </AlertDialogAction>
              </AlertDialogFooter>
            </AlertDialogContent>
          </AlertDialog>
        </div>
        {(analysis || action) && (
          <p role='status' className='text-muted-foreground'>
            {analysis
              ? analysis.glossary?.total
                ? t(
                    analysis.glossary.phase === 'merge'
                      ? 'glossary.mergeBatchProgress'
                      : 'glossary.batchProgress',
                    analysis.glossary,
                  )
                : t('glossary.analyzing')
              : action}
          </p>
        )}
        {checked && (
          <section className='space-y-2' aria-label={t('glossary.check')}>
            {!data?.issues.length && <p role='status'>{t('glossary.noIssues')}</p>}
            {data?.issues.map((issue) => (
              <button
                type='button'
                key={`${issue.layer}:${issue.source}`}
                onClick={() => openLine(issue.page, issue.layer)}
                className='block w-full rounded border px-2 py-1.5 text-left hover:bg-muted'
              >
                <span className='block font-medium'>
                  {t('glossary.page', { page: issue.number })} · {issue.source} → {issue.expected}
                </span>
                {issue.translation && (
                  <span className='block wrap-anywhere text-muted-foreground'>
                    {issue.translation}
                  </span>
                )}
              </button>
            ))}
          </section>
        )}
        <Dialog open={managerOpen} onOpenChange={setManagerOpen}>
          <DialogTrigger render={<Button size='sm' disabled={!safe} />}>
            <BookOpenText /> {t('glossary.manage')}
          </DialogTrigger>
          <DialogContent
            showCloseButton={false}
            className='flex h-[min(720px,calc(100dvh-2rem))] min-h-0 max-w-[calc(100%-2rem)] flex-col p-5 sm:max-w-4xl'
          >
            <DialogHeader className='relative shrink-0 border-b pb-3'>
              <DialogTitle>{t('glossary.manage')}</DialogTitle>
              <DialogClose
                render={
                  <Button
                    size='icon-sm'
                    variant='ghost'
                    className='absolute -end-1 -top-2'
                    aria-label={t('window.close')}
                  />
                }
              >
                <X />
              </DialogClose>
            </DialogHeader>
            <GlossaryManager
              entries={list}
              candidates={candidates ?? []}
              disabled={!safe || busy || pipelineBusy}
              onEditCandidate={async (source, translation, remove) => {
                if (!safe) return
                setBusy(true)
                try {
                  await call(commands.editManualCandidate, project, source, translation, remove)
                  await refresh(glossaryKey)
                } finally {
                  setBusy(false)
                }
              }}
              onApplyCandidate={async (entry, locations) => {
                setBusy(true)
                try {
                  const result = await call(commands.applyCandidate, project, entry, locations)
                  await refresh(glossaryKey)
                  return result
                } finally {
                  setBusy(false)
                }
              }}
              onSave={save}
              onLocation={(location) => {
                setManagerOpen(false)
                openLine(location.page, location.layer)
              }}
            />
          </DialogContent>
        </Dialog>
        <dl className='grid grid-cols-3 gap-2 rounded-lg border px-3 py-2.5'>
          {[
            ['glossary.title', list.length - blockedCount],
            ['glossary.blockedTable', blockedCount],
            ['glossary.candidates', candidates?.length ?? 0],
          ].map(([label, count]) => (
            <div key={label} className='min-w-0'>
              <dt className='text-xs text-muted-foreground'>{t(String(label))}</dt>
              <dd className='mt-0.5 font-medium tabular-nums'>{count}</dd>
            </div>
          ))}
        </dl>
      </div>
    </ScrollArea>
  )
}
