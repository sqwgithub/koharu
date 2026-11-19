'use client'

import { useState } from 'react'
import { useTranslation } from 'react-i18next'

import { ColorWell } from '@/components/controls/ColorWell'
import { call } from '@/lib/backend'
import { hexToRgba } from '@/lib/store'
import { commands, type ProjectId } from '@koharu/ipc/protocol'
import { Button } from '@koharu/ui/components/button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@koharu/ui/components/dialog'
import { Input } from '@koharu/ui/components/input'

export function NormalizeInkDialog({
  project,
  pageCount,
  onClose,
}: {
  project: ProjectId
  pageCount: number
  onClose: () => void
}) {
  const { t } = useTranslation()
  const [blackMax, setBlackMax] = useState('24')
  const [whiteMin, setWhiteMin] = useState('231')
  const [black, setBlack] = useState('#000000')
  const [white, setWhite] = useState('#FFFFFF')
  const [busy, setBusy] = useState(false)
  const [changed, setChanged] = useState<number | null>(null)
  const max = Number(blackMax)
  const min = Number(whiteMin)
  const valid =
    blackMax.trim() !== '' &&
    whiteMin.trim() !== '' &&
    Number.isInteger(max) &&
    Number.isInteger(min) &&
    max >= 0 &&
    max < min &&
    min <= 255
  const apply = async (restore: boolean) => {
    if (busy || !pageCount || (!restore && !valid)) return
    setBusy(true)
    setChanged(null)
    try {
      const settings = restore
        ? null
        : {
            black_max: max,
            white_min: min,
            black: hexToRgba(black),
            white: hexToRgba(white),
          }
      setChanged(await call(commands.normalizeInk, project, settings))
    } finally {
      setBusy(false)
    }
  }
  return (
    <Dialog
      open
      onOpenChange={(open) => {
        if (!open && !busy) onClose()
      }}
    >
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{t('ink.title')}</DialogTitle>
          <DialogDescription>{t('ink.scope', { pages: pageCount })}</DialogDescription>
        </DialogHeader>
        <details className='rounded-md border p-3 text-sm'>
          <summary className='cursor-pointer font-medium'>{t('ink.advanced')}</summary>
          <div className='mt-3 grid grid-cols-[minmax(0,1fr)_auto] gap-x-4 gap-y-3'>
            {(
              [
                [t('ink.blackThreshold'), blackMax, setBlackMax, black, setBlack],
                [t('ink.whiteThreshold'), whiteMin, setWhiteMin, white, setWhite],
              ] as const
            ).map(([label, value, setValue, color, setColor]) => (
              <div key={label} className='col-span-2 grid grid-cols-subgrid items-end'>
                <label className='grid gap-1'>
                  {label}
                  <Input
                    type='number'
                    min={0}
                    max={255}
                    step={1}
                    value={value}
                    onChange={(e) => setValue(e.target.value)}
                    disabled={busy}
                  />
                </label>
                <div className='grid gap-1'>
                  <span>{t('ink.target')}</span>
                  <div className='flex h-9 items-center gap-2'>
                    <ColorWell
                      size='sm'
                      label={label + ' · ' + t('ink.target')}
                      value={color}
                      onChange={setColor}
                      disabled={busy}
                    />
                    <code>{color}</code>
                  </div>
                </div>
              </div>
            ))}
            {!valid && (
              <p role='alert' className='col-span-2 text-destructive'>
                {t('ink.invalid')}
              </p>
            )}
          </div>
        </details>
        {changed !== null && (
          <p role='status' className='text-sm text-muted-foreground'>
            {t('ink.changed', { blocks: changed })}
          </p>
        )}
        <DialogFooter>
          <Button
            variant='outline'
            disabled={busy || !pageCount}
            onClick={() => void apply(true).catch(() => undefined)}
          >
            {t('ink.restore')}
          </Button>
          <Button
            disabled={busy || !valid || !pageCount}
            onClick={() => void apply(false).catch(() => undefined)}
          >
            {t('ink.apply')}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
