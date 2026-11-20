'use client'

import { useTranslation } from 'react-i18next'

import {
  PreferencePage,
  PreferenceRow,
  PreferenceSection,
} from '@/components/preferences/PreferenceFields'
import type { OutputConfig } from '@koharu/ipc/protocol'
import { Slider } from '@koharu/ui/components/slider'
import { Switch } from '@koharu/ui/components/switch'

export function OutputPreferences({
  value,
  onChange,
}: {
  value: OutputConfig
  onChange: (value: OutputConfig) => void
}) {
  const { t } = useTranslation()
  return (
    <PreferencePage
      title={t('settings.tabs.output')}
      description={t('settings.output.description')}
    >
      <PreferenceSection title={t('settings.output.images')}>
        {(['scale_percent', 'jpeg_quality'] as const).map((key) => {
          const scale = key === 'scale_percent'
          const title = t(scale ? 'settings.output.scale' : 'settings.output.jpegQuality')
          const current = value[key] ?? (scale ? 100 : 95)
          return (
            <PreferenceRow
              key={key}
              title={title}
              description={t(
                scale ? 'settings.output.scaleDescription' : 'settings.output.jpegDescription',
              )}
            >
              <div className='flex items-center gap-3'>
                <Slider
                  aria-label={title}
                  min={1}
                  max={100}
                  step={1}
                  value={current}
                  onValueChange={(next) => onChange({ ...value, [key]: next })}
                />
                <span className='w-10 shrink-0 text-right text-xs tabular-nums'>
                  {current}
                  {scale ? '%' : ''}
                </span>
              </div>
            </PreferenceRow>
          )
        })}
        <PreferenceRow
          title={t('settings.output.auto')}
          description={t('settings.output.autoDescription')}
        >
          <Switch
            aria-label={t('settings.output.auto')}
            checked={value.auto_compress ?? false}
            onCheckedChange={(auto_compress) => onChange({ ...value, auto_compress })}
          />
        </PreferenceRow>
      </PreferenceSection>
    </PreferencePage>
  )
}
