'use client'

import { ScanText, Sparkles, X } from 'lucide-react'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'

import { ContextBar } from '@/components/editor/ContextBar'
import { inpainters } from '@/components/preferences/models'
import { dispatch, savePreferences } from '@/lib/backend'
import { useKoharuStore, type CanvasTool } from '@/lib/store'
import { commands, type Inpainter } from '@koharu/ipc/protocol'
import { Button } from '@koharu/ui/components/button'
import { Input } from '@koharu/ui/components/input'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@koharu/ui/components/select'
import { Tooltip, TooltipContent, TooltipTrigger } from '@koharu/ui/components/tooltip'

/** The tools whose area the bar follows. */
const TOOLS: CanvasTool[] = ['marquee', 'lasso']

/** Area actions live below the selection. Generative Fill has its own model preference. */
export function FillBar() {
  const { t } = useTranslation()
  const area = useKoharuStore((state) => state.area)
  const tool = useKoharuStore((state) => state.tool)
  const preferences = useKoharuStore((state) => state.preferences)
  // Kept after Generate, to fill again with it, as Photoshop keeps it.
  const [prompt, setPrompt] = useState('')
  if (!area || !preferences || !TOOLS.includes(tool)) return null

  const pipeline = preferences.pipeline
  const model = pipeline.inpainter?.fill ?? 'flux3-klein'
  const prompted = model !== 'lama'
  const generate = () => dispatch(commands.fill, (prompted && prompt.trim()) || null, model)
  return (
    <ContextBar bounds={area} aria-label={t(`tools.${tool}`)}>
      {tool === 'marquee' && (
        <Button
          type='button'
          variant='secondary'
          size='sm'
          className='h-7 text-[11px]'
          onClick={() => dispatch(commands.recognizeSelection)}
        >
          <ScanText />
          {t('phase.recognize')}
        </Button>
      )}
      <Select
        value={model}
        items={inpainters}
        onValueChange={(fill) =>
          fill &&
          void savePreferences({
            ...preferences,
            pipeline: {
              ...pipeline,
              inpainter: { ...pipeline.inpainter, fill: fill as Inpainter },
            },
          })
        }
      >
        <SelectTrigger size='sm' aria-label={t('fill.model')} className='h-7 text-[11px]'>
          <SelectValue />
        </SelectTrigger>
        <SelectContent
          align='start'
          alignItemWithTrigger={false}
          // A pick with the pointer leaves the keys to the canvas, as Photoshop's options bar
          // does, so a tool's key that follows switches the tool, not the model by its name.
          finalFocus={(closed) => closed === 'keyboard'}
          className='w-auto min-w-36 rounded-xl border border-border/50 p-1 shadow-sm ring-0'
        >
          {(Object.keys(inpainters) as Inpainter[]).map((name) => (
            <SelectItem key={name} value={name} className='min-h-7 pr-7 pl-2 *:[span]:last:right-2'>
              {inpainters[name]}
            </SelectItem>
          ))}
        </SelectContent>
      </Select>
      {prompted && (
        <Input
          value={prompt}
          aria-label={t('fill.prompt')}
          placeholder={t('fill.prompt')}
          className='h-7 w-60 text-[11px]'
          onChange={(event) => setPrompt(event.target.value)}
          onKeyDown={(event) => {
            if (event.key === 'Enter' && !event.nativeEvent.isComposing) {
              event.preventDefault()
              generate()
            }
          }}
        />
      )}
      <Button type='button' size='sm' className='h-7 text-[11px]' onClick={generate}>
        <Sparkles />
        {t('fill.generate')}
      </Button>
      <Tooltip>
        <TooltipTrigger
          render={
            <Button
              type='button'
              variant='ghost'
              size='icon-sm'
              aria-label={t('fill.deselect')}
              className='text-muted-foreground hover:text-foreground'
              onClick={() => dispatch(commands.deselect)}
            />
          }
        >
          <X className='size-3.5' />
        </TooltipTrigger>
        <TooltipContent side='bottom'>
          {t('fill.deselect')}
          <span className='ml-2 opacity-60'>Ctrl+D</span>
        </TooltipContent>
      </Tooltip>
    </ContextBar>
  )
}
