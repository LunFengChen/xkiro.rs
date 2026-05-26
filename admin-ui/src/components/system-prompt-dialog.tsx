import { useEffect, useMemo, useState } from 'react'
import { Loader2, Plus, Trash2, FileText, Sparkles, User, Wand2 } from 'lucide-react'
import { toast } from 'sonner'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Switch } from '@/components/ui/switch'
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import {
  useSystemPrompt,
  useUpdateSystemPrompt,
  useUpsertUserPreset,
  useDeleteUserPreset,
} from '@/hooks/use-system-prompt'
import type {
  PresetItem,
  SystemPromptPosition,
  UpsertUserPresetRequest,
} from '@/types/api'
import { extractErrorMessage } from '@/lib/utils'

interface SystemPromptDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
}

type TabId = 'general' | 'builtin' | 'user' | 'custom'

const TABS: { id: TabId; label: string; icon: React.ReactNode }[] = [
  { id: 'general', label: '基础', icon: <Sparkles className="h-4 w-4" /> },
  { id: 'builtin', label: '内置', icon: <FileText className="h-4 w-4" /> },
  { id: 'user', label: '用户', icon: <User className="h-4 w-4" /> },
  { id: 'custom', label: '自定义', icon: <Wand2 className="h-4 w-4" /> },
]

const ID_REGEX = /^[a-z0-9_-]{1,32}$/

export function SystemPromptDialog({ open, onOpenChange }: SystemPromptDialogProps) {
  const [activeTab, setActiveTab] = useState<TabId>('general')
  const { data, isLoading } = useSystemPrompt(open)
  const update = useUpdateSystemPrompt()
  const upsert = useUpsertUserPreset()
  const remove = useDeleteUserPreset()

  const [enabled, setEnabled] = useState(false)
  const [position, setPosition] = useState<SystemPromptPosition>('append')
  const [customContent, setCustomContent] = useState('')
  const [enabledIds, setEnabledIds] = useState<Set<string>>(new Set())

  // user preset form
  const [presetId, setPresetId] = useState('')
  const [presetName, setPresetName] = useState('')
  const [presetDesc, setPresetDesc] = useState('')
  const [presetContent, setPresetContent] = useState('')
  const [editingId, setEditingId] = useState<string | null>(null)

  useEffect(() => {
    if (!data) return
    setEnabled(data.enabled)
    setPosition(data.position)
    setCustomContent(data.customContent ?? '')
    setEnabledIds(new Set(data.presets.filter(p => p.enabled).map(p => p.id)))
  }, [data])

  const builtinPresets = useMemo(
    () => (data?.presets ?? []).filter(p => p.source === 'builtin'),
    [data],
  )
  const userPresets = useMemo(
    () => (data?.presets ?? []).filter(p => p.source === 'user'),
    [data],
  )

  const togglePreset = (id: string) => {
    setEnabledIds(prev => {
      const next = new Set(prev)
      if (next.has(id)) next.delete(id)
      else next.add(id)
      return next
    })
  }

  const handleSaveGeneral = async () => {
    try {
      await update.mutateAsync({
        enabled,
        position,
        customContent,
        enabledPresets: Array.from(enabledIds),
      })
      toast.success('已保存，立即生效')
    } catch (e) {
      toast.error(`保存失败: ${extractErrorMessage(e)}`)
    }
  }

  const resetForm = () => {
    setPresetId('')
    setPresetName('')
    setPresetDesc('')
    setPresetContent('')
    setEditingId(null)
  }

  const startEdit = (p: PresetItem) => {
    setEditingId(p.id)
    setPresetId(p.id)
    setPresetName(p.name)
    setPresetDesc(p.description)
    setPresetContent(p.content ?? '')
  }

  const handleUpsert = async () => {
    const id = presetId.trim()
    if (!ID_REGEX.test(id)) {
      toast.error('id 仅允许 [a-z0-9_-]，长度 1-32')
      return
    }
    if (!presetName.trim()) {
      toast.error('name 不能为空')
      return
    }
    if (!presetContent.trim()) {
      toast.error('content 不能为空')
      return
    }
    const req: UpsertUserPresetRequest = {
      id,
      name: presetName.trim(),
      description: presetDesc,
      content: presetContent,
    }
    try {
      await upsert.mutateAsync(req)
      toast.success(editingId ? '已更新' : '已新增')
      resetForm()
    } catch (e) {
      toast.error(`保存失败: ${extractErrorMessage(e)}`)
    }
  }

  const handleDelete = async (id: string) => {
    if (!confirm(`删除用户预设 "${id}"？`)) return
    try {
      await remove.mutateAsync(id)
      toast.success('已删除')
      if (editingId === id) resetForm()
    } catch (e) {
      toast.error(`删除失败: ${extractErrorMessage(e)}`)
    }
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-3xl p-0 gap-0 h-[80vh] max-h-[720px] flex flex-col overflow-hidden">
        <DialogHeader className="px-6 pt-5 pb-4 border-b shrink-0">
          <DialogTitle>系统提示注入</DialogTitle>
        </DialogHeader>

        <div className="flex flex-1 min-h-0 overflow-hidden">
          {/* 左侧导航 */}
          <nav className="w-32 shrink-0 border-r bg-muted/30 py-2 overflow-y-auto">
            {TABS.map(tab => (
              <button
                key={tab.id}
                onClick={() => setActiveTab(tab.id)}
                className={`w-full flex items-center gap-2 px-4 py-2 text-sm transition-colors ${
                  activeTab === tab.id
                    ? 'bg-background text-foreground font-medium border-r-2 border-primary'
                    : 'text-muted-foreground hover:text-foreground hover:bg-background/50'
                }`}
              >
                {tab.icon}
                <span>{tab.label}</span>
              </button>
            ))}
          </nav>

          {/* 右侧内容 */}
          <div className="flex-1 flex flex-col min-h-0">
            <div className="flex-1 overflow-y-auto px-6 py-4 max-h-[60vh]">
              {isLoading || !data ? (
                <div className="flex items-center justify-center py-10 text-sm text-muted-foreground">
                  <Loader2 className="h-4 w-4 animate-spin mr-2" /> 加载中
                </div>
              ) : activeTab === 'general' ? (
                <GeneralTab
                  enabled={enabled}
                  setEnabled={setEnabled}
                  position={position}
                  setPosition={setPosition}
                />
              ) : activeTab === 'builtin' ? (
                <PresetList
                  presets={builtinPresets}
                  enabledIds={enabledIds}
                  onToggle={togglePreset}
                />
              ) : activeTab === 'user' ? (
                <UserTab
                  presets={userPresets}
                  enabledIds={enabledIds}
                  onToggle={togglePreset}
                  onEdit={startEdit}
                  onDelete={handleDelete}
                  presetId={presetId}
                  setPresetId={setPresetId}
                  presetName={presetName}
                  setPresetName={setPresetName}
                  presetDesc={presetDesc}
                  setPresetDesc={setPresetDesc}
                  presetContent={presetContent}
                  setPresetContent={setPresetContent}
                  editingId={editingId}
                  onUpsert={handleUpsert}
                  onResetForm={resetForm}
                  saving={upsert.isPending}
                />
              ) : (
                <CustomTab content={customContent} setContent={setCustomContent} />
              )}
            </div>

            {/* 底部操作栏 */}
            {data && (
              <div className="flex gap-2 px-6 py-3 border-t bg-muted/20">
                <Button
                  onClick={handleSaveGeneral}
                  disabled={update.isPending}
                  size="sm"
                  className="flex-1"
                >
                  {update.isPending && <Loader2 className="h-4 w-4 animate-spin mr-2" />}
                  保存（立即生效）
                </Button>
              </div>
            )}
          </div>
        </div>
      </DialogContent>
    </Dialog>
  )
}

function GeneralTab({
  enabled,
  setEnabled,
  position,
  setPosition,
}: {
  enabled: boolean
  setEnabled: (v: boolean) => void
  position: SystemPromptPosition
  setPosition: (v: SystemPromptPosition) => void
}) {
  return (
    <div className="space-y-1">
      <div className="flex items-center justify-between gap-4 py-2">
        <div>
          <div className="text-sm font-medium">启用注入</div>
          <p className="text-xs text-muted-foreground mt-0.5">
            关闭后所有 preset / 自定义内容都不生效
          </p>
        </div>
        <Switch checked={enabled} onCheckedChange={setEnabled} />
      </div>
      <div className="flex items-center justify-between gap-4 py-2">
        <div>
          <div className="text-sm font-medium">注入位置</div>
          <p className="text-xs text-muted-foreground mt-0.5">
            prepend = 注入到 system 列表头部；append = 追加到尾部
          </p>
        </div>
        <select
          value={position}
          onChange={e => setPosition(e.target.value as SystemPromptPosition)}
          className="h-8 rounded-md border border-input bg-background px-3 text-sm"
        >
          <option value="append">append</option>
          <option value="prepend">prepend</option>
        </select>
      </div>
      <p className="mt-4 text-xs text-muted-foreground leading-relaxed">
        最终注入文本顺序：内置预设 → 用户预设 → 自定义内容（按 \n\n 拼接）。
        所有片段都启用且非空才会出现。
      </p>
    </div>
  )
}

function PresetList({
  presets,
  enabledIds,
  onToggle,
}: {
  presets: PresetItem[]
  enabledIds: Set<string>
  onToggle: (id: string) => void
}) {
  if (presets.length === 0) {
    return <p className="text-sm text-muted-foreground py-6">暂无内置预设</p>
  }
  return (
    <div className="space-y-2">
      {presets.map(p => (
        <div
          key={p.id}
          className="flex items-start justify-between gap-4 rounded-md border bg-card px-3 py-2"
        >
          <div className="flex-1 min-w-0">
            <div className="flex items-center gap-2">
              <span className="text-sm font-medium">{p.name}</span>
              <span className="text-xs text-muted-foreground">{p.id}</span>
            </div>
            <p className="text-xs text-muted-foreground mt-1 leading-relaxed">
              {p.description}
            </p>
          </div>
          <Switch
            checked={enabledIds.has(p.id)}
            onCheckedChange={() => onToggle(p.id)}
          />
        </div>
      ))}
    </div>
  )
}

function UserTab(props: {
  presets: PresetItem[]
  enabledIds: Set<string>
  onToggle: (id: string) => void
  onEdit: (p: PresetItem) => void
  onDelete: (id: string) => void
  presetId: string
  setPresetId: (v: string) => void
  presetName: string
  setPresetName: (v: string) => void
  presetDesc: string
  setPresetDesc: (v: string) => void
  presetContent: string
  setPresetContent: (v: string) => void
  editingId: string | null
  onUpsert: () => void
  onResetForm: () => void
  saving: boolean
}) {
  return (
    <div className="space-y-4">
      <div className="space-y-2">
        <div className="text-xs font-medium text-muted-foreground uppercase">已保存预设</div>
        {props.presets.length === 0 ? (
          <p className="text-sm text-muted-foreground py-2">暂无用户预设</p>
        ) : (
          props.presets.map(p => (
            <div
              key={p.id}
              className="flex items-start justify-between gap-3 rounded-md border bg-card px-3 py-2"
            >
              <div className="flex-1 min-w-0">
                <div className="flex items-center gap-2">
                  <span className="text-sm font-medium">{p.name}</span>
                  <span className="text-xs text-muted-foreground">{p.id}</span>
                </div>
                {p.description && (
                  <p className="text-xs text-muted-foreground mt-1 leading-relaxed line-clamp-2">
                    {p.description}
                  </p>
                )}
              </div>
              <div className="flex items-center gap-2 shrink-0">
                <Switch
                  checked={props.enabledIds.has(p.id)}
                  onCheckedChange={() => props.onToggle(p.id)}
                />
                <Button
                  variant="outline"
                  size="sm"
                  onClick={() => props.onEdit(p)}
                  className="h-7 px-2 text-xs"
                >
                  编辑
                </Button>
                <Button
                  variant="ghost"
                  size="icon"
                  onClick={() => props.onDelete(p.id)}
                  className="h-7 w-7 text-destructive hover:text-destructive"
                >
                  <Trash2 className="h-3.5 w-3.5" />
                </Button>
              </div>
            </div>
          ))
        )}
      </div>

      <div className="space-y-2 rounded-md border bg-muted/20 p-3">
        <div className="flex items-center justify-between">
          <div className="text-xs font-medium text-muted-foreground uppercase">
            {props.editingId ? `编辑 ${props.editingId}` : '新增预设'}
          </div>
          {props.editingId && (
            <Button
              variant="ghost"
              size="sm"
              onClick={props.onResetForm}
              className="h-6 px-2 text-xs"
            >
              取消编辑
            </Button>
          )}
        </div>
        <div className="grid grid-cols-2 gap-2">
          <Input
            placeholder="id (a-z0-9_-, 1-32)"
            value={props.presetId}
            onChange={e => props.setPresetId(e.target.value)}
            disabled={props.editingId !== null}
            className="h-8 text-sm"
          />
          <Input
            placeholder="名称"
            value={props.presetName}
            onChange={e => props.setPresetName(e.target.value)}
            className="h-8 text-sm"
          />
        </div>
        <Input
          placeholder="描述（可选）"
          value={props.presetDesc}
          onChange={e => props.setPresetDesc(e.target.value)}
          className="h-8 text-sm"
        />
        <textarea
          placeholder="prompt 正文"
          value={props.presetContent}
          onChange={e => props.setPresetContent(e.target.value)}
          className="w-full min-h-[140px] rounded-md border border-input bg-background px-3 py-2 text-sm font-mono"
        />
        <div className="flex justify-end gap-2">
          <Button
            size="sm"
            onClick={props.onUpsert}
            disabled={props.saving}
          >
            {props.saving ? (
              <Loader2 className="h-4 w-4 animate-spin mr-2" />
            ) : (
              <Plus className="h-4 w-4 mr-1" />
            )}
            {props.editingId ? '更新' : '保存'}
          </Button>
        </div>
      </div>
    </div>
  )
}

function CustomTab({
  content,
  setContent,
}: {
  content: string
  setContent: (v: string) => void
}) {
  return (
    <div className="space-y-2">
      <div className="text-xs font-medium text-muted-foreground uppercase">
        自定义内容
      </div>
      <p className="text-xs text-muted-foreground leading-relaxed">
        填入额外文本，注入时拼到所有 preset 之后。留空则不注入此段。
      </p>
      <textarea
        value={content}
        onChange={e => setContent(e.target.value)}
        placeholder="任意 prompt 内容…"
        className="w-full min-h-[260px] rounded-md border border-input bg-background px-3 py-2 text-sm font-mono"
      />
    </div>
  )
}
