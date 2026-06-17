import { useEffect, useMemo, useState } from 'react'
import { toast } from 'sonner'
import {
  Loader2,
  Plus,
  Trash2,
  Wifi,
  Pencil,
  X,
  Wand2,
  ChevronDown,
  ChevronRight,
} from 'lucide-react'
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
} from '@/components/ui/dialog'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Switch } from '@/components/ui/switch'
import { Badge } from '@/components/ui/badge'
import { Tooltip, TooltipContent, TooltipTrigger } from '@/components/ui/tooltip'
import {
  useProxies,
  useAddProxy,
  useUpdateProxy,
  useDeleteProxy,
  useTestProxy,
  useImportProxies,
  useAutoAssignProxies,
} from '@/hooks/use-proxies'
import type { ProxyItem, ProxyUpsertRequest } from '@/types/api'
import { extractErrorMessage } from '@/lib/utils'

interface ProxyPoolDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
}

interface FormState {
  url: string
  username: string
  password: string
  region: string
  maxConcurrency: string
  note: string
  disabled: boolean
}

const EMPTY_FORM: FormState = {
  url: '',
  username: '',
  password: '',
  region: '',
  maxConcurrency: '',
  note: '',
  disabled: false,
}

/** 展示用：剥掉 URL 里的账号密码，只保留 协议//host:port */
function stripCreds(url: string): string {
  try {
    const u = new URL(url)
    return `${u.protocol}//${u.hostname}${u.port ? ':' + u.port : ''}`
  } catch {
    // 解析失败时退回正则兜底（去掉 user:pass@）
    return url.replace(/^([a-z][a-z0-9+.-]*:\/\/)[^/@]*@/i, '$1')
  }
}

/** 国家两字母码 → 国旗 emoji；空值回退地球 */
function countryFlag(code?: string | null): string {
  const c = code?.trim().toUpperCase()
  if (!c || c.length !== 2 || !/^[A-Z]{2}$/.test(c)) return '🌐'
  return String.fromCodePoint(
    ...[...c].map((ch) => 0x1f1e6 + ch.charCodeAt(0) - 65),
  )
}

/** 单行：若无已知 scheme 则补 socks5:// */
function ensureProxyScheme(line: string): string {
  const t = line.trim()
  if (!t) return t
  if (/^(https?|socks[45]?):\/\//i.test(t)) return t
  return 'socks5://' + t
}

/** 批量文本：对每行补 scheme，空行/注释行保留原样 */
function ensureProxySchemeBlock(text: string): string {
  return text
    .split('\n')
    .map((l) => (l.trim() === '' || l.trimStart().startsWith('#') ? l : ensureProxyScheme(l)))
    .join('\n')
}

function formStateToRequest(form: FormState): ProxyUpsertRequest {
  const req: ProxyUpsertRequest = {
    url: form.url.trim(),
    disabled: form.disabled,
  }
  if (form.username.trim()) req.username = form.username.trim()
  if (form.password) req.password = form.password
  if (form.region.trim()) req.region = form.region.trim()
  if (form.note.trim()) req.note = form.note.trim()
  const mc = parseInt(form.maxConcurrency.trim(), 10)
  if (!isNaN(mc) && mc >= 1) req.maxConcurrency = mc
  return req
}

export function ProxyPoolDialog({ open, onOpenChange }: ProxyPoolDialogProps) {
  const { data, isLoading } = useProxies()
  const addProxy = useAddProxy()
  const updateProxy = useUpdateProxy()
  const deleteProxy = useDeleteProxy()
  const testProxy = useTestProxy()
  const importProxies = useImportProxies()
  const autoAssign = useAutoAssignProxies()

  // 新增/编辑表单
  const [form, setForm] = useState<FormState>(EMPTY_FORM)
  const [editingId, setEditingId] = useState<number | null>(null)
  // 单行测试中的代理 id
  const [testingId, setTestingId] = useState<number | null>(null)
  // 删除确认目标
  const [deleteTarget, setDeleteTarget] = useState<ProxyItem | null>(null)

  // 批量导入
  const [importText, setImportText] = useState('')
  const [importRegion, setImportRegion] = useState('')
  const [importMaxConcurrency, setImportMaxConcurrency] = useState('')

  const proxies = data?.proxies ?? []

  // Group proxies by region (ungrouped → '未分组')
  const groupedProxies = useMemo(() => {
    const map = new Map<string, typeof proxies>()
    for (const p of proxies) {
      const key = p.region?.trim() || '未分组'
      if (!map.has(key)) map.set(key, [])
      map.get(key)!.push(p)
    }
    // Sort: named groups first (alphabetical), then '未分组'
    return [...map.entries()].sort(([a], [b]) => {
      if (a === '未分组') return 1
      if (b === '未分组') return -1
      return a.localeCompare(b)
    })
  }, [proxies])

  const [collapsedGroups, setCollapsedGroups] = useState<Set<string>>(new Set())
  const toggleGroup = (group: string) => {
    setCollapsedGroups((prev) => {
      const next = new Set(prev)
      if (next.has(group)) next.delete(group)
      else next.add(group)
      return next
    })
  }

  useEffect(() => {
    if (!open) {
      // 关闭时重置表单状态
      setForm(EMPTY_FORM)
      setEditingId(null)
      setImportText('')
      setImportRegion('')
      setImportMaxConcurrency('')
    }
  }, [open])

  const startEdit = (p: ProxyItem) => {
    setEditingId(p.id)
    setForm({
      url: p.url,
      username: p.username ?? '',
      password: '',
      region: p.region ?? '',
      maxConcurrency: String(p.maxConcurrency),
      note: p.note ?? '',
      disabled: p.disabled,
    })
  }

  const cancelEdit = () => {
    setEditingId(null)
    setForm(EMPTY_FORM)
  }

  const handleSubmitForm = () => {
    if (!form.url.trim()) {
      toast.error('代理 URL 不能为空')
      return
    }
    const req = formStateToRequest(form)
    if (editingId != null) {
      updateProxy.mutate(
        { id: editingId, req },
        {
          onSuccess: (res) => {
            toast.success(res.message || '已更新代理')
            cancelEdit()
          },
          onError: (err) => toast.error(`更新失败: ${extractErrorMessage(err)}`),
        },
      )
    } else {
      addProxy.mutate(req, {
        onSuccess: (res) => {
          toast.success(res.message || '已新增代理')
          setForm(EMPTY_FORM)
        },
        onError: (err) => toast.error(`新增失败: ${extractErrorMessage(err)}`),
      })
    }
  }

  const handleTest = (id: number) => {
    setTestingId(id)
    testProxy.mutate(id, {
      onSuccess: (res) => {
        if (res.ok) {
          toast.success(
            `连通正常 出口IP ${res.exitIp ?? '未知'}${
              res.latencyMs != null ? ` · ${res.latencyMs}ms` : ''
            }`,
          )
        } else {
          toast.error(`测试失败: ${res.error ?? '未知错误'}`)
        }
      },
      onError: (err) => toast.error(`测试失败: ${extractErrorMessage(err)}`),
      onSettled: () => setTestingId(null),
    })
  }

  const handleDelete = () => {
    if (!deleteTarget) return
    deleteProxy.mutate(deleteTarget.id, {
      onSuccess: (res) => {
        toast.success(res.message || '已删除代理')
        setDeleteTarget(null)
      },
      onError: (err) => toast.error(`删除失败: ${extractErrorMessage(err)}`),
    })
  }

  const handleImport = () => {
    const text = importText.trim()
    if (!text) {
      toast.error('请输入要导入的代理（每行一个）')
      return
    }
    const mc = parseInt(importMaxConcurrency.trim(), 10)
    importProxies.mutate(
      {
        text,
        region: importRegion.trim() || undefined,
        maxConcurrency: !isNaN(mc) && mc >= 1 ? mc : undefined,
      },
      {
        onSuccess: (res) => {
          toast.success(`导入完成：成功 ${res.added}，失败 ${res.failed}`)
          if (res.errors.length > 0) {
            toast.error(res.errors.slice(0, 5).join('\n'))
          }
          if (res.added > 0) setImportText('')
        },
        onError: (err) => toast.error(`导入失败: ${extractErrorMessage(err)}`),
      },
    )
  }

  const handleAutoAssign = () => {
    autoAssign.mutate(
      { credentialIds: [], reassignBound: false },
      {
        onSuccess: (res) => {
          toast.success(
            `自动分配完成：已分配 ${res.assigned.length}，跳过 ${res.skipped.length}`,
          )
        },
        onError: (err) => toast.error(`自动分配失败: ${extractErrorMessage(err)}`),
      },
    )
  }

  const formPending = addProxy.isPending || updateProxy.isPending

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-3xl max-h-[88vh] overflow-y-auto">
        <DialogHeader>
          <DialogTitle>代理池管理</DialogTitle>
          <DialogDescription>
            管理出口代理、绑定凭据并监控健康状态。
          </DialogDescription>
        </DialogHeader>

        {/* === 代理列表 === */}
        <div className="space-y-2">
          <div className="flex items-center justify-between">
            <h3 className="text-sm font-medium">
              代理列表
              <span className="ml-1.5 text-xs text-muted-foreground">
                共 {proxies.length} 个
              </span>
            </h3>
            <Button
              size="sm"
              variant="outline"
              className="h-8"
              onClick={handleAutoAssign}
              disabled={autoAssign.isPending}
            >
              {autoAssign.isPending ? (
                <Loader2 className="mr-1 h-3.5 w-3.5 animate-spin" />
              ) : (
                <Wand2 className="mr-1 h-3.5 w-3.5" />
              )}
              自动分配
            </Button>
          </div>

          {isLoading ? (
            <div className="flex items-center justify-center py-8 text-sm text-muted-foreground">
              <Loader2 className="mr-2 h-4 w-4 animate-spin" /> 加载中...
            </div>
          ) : proxies.length === 0 ? (
            <div className="py-8 text-center text-sm text-muted-foreground">
              暂无代理，请在下方新增或批量导入。
            </div>
          ) : (
            <div className="space-y-2">
              {groupedProxies.map(([group, items]) => {
                const collapsed = collapsedGroups.has(group)
                const groupCountry = items.find((p) => p.country)?.country
                const onlineCount = items.filter((p) => !p.dead && !p.disabled).length
                const disabledCount = items.filter((p) => p.disabled).length
                const deadCount = items.filter((p) => p.dead).length
                return (
                  <div key={group} className="overflow-hidden rounded-md border">
                    {/* Group header */}
                    <button
                      type="button"
                      className="flex w-full items-center gap-2 bg-muted/40 px-3 py-2 text-sm font-medium transition-colors hover:bg-muted/60"
                      onClick={() => toggleGroup(group)}
                    >
                      {collapsed ? (
                        <ChevronRight className="h-3.5 w-3.5 shrink-0 text-muted-foreground" />
                      ) : (
                        <ChevronDown className="h-3.5 w-3.5 shrink-0 text-muted-foreground" />
                      )}
                      <span className="text-base leading-none">
                        {group === '未分组' ? '🌐' : countryFlag(groupCountry)}
                      </span>
                      <span className="font-semibold">{group}</span>
                      <Badge variant="secondary" className="shrink-0 font-normal">
                        {items.length}
                      </Badge>
                      <span className="ml-auto flex items-center gap-1.5 text-xs font-normal">
                        {onlineCount > 0 && (
                          <Badge variant="success" className="shrink-0">
                            在线 {onlineCount}
                          </Badge>
                        )}
                        {disabledCount > 0 && (
                          <Badge variant="secondary" className="shrink-0">
                            禁用 {disabledCount}
                          </Badge>
                        )}
                        {deadCount > 0 && (
                          <Badge variant="destructive" className="shrink-0">
                            离线 {deadCount}
                          </Badge>
                        )}
                      </span>
                    </button>

                    {/* Proxy rows */}
                    {!collapsed && (
                      <div className="divide-y border-t">
                        {items.map((p) => {
                          return (
                          <div
                            key={p.id}
                            className="flex items-center gap-3 bg-muted/10 px-3 py-2 text-xs"
                          >
                            {/* 状态点：在线绿色带辉光 / 禁用黄 / 离线红 */}
                            <span
                              className={
                                'mt-0.5 h-2 w-2 shrink-0 self-start rounded-full ' +
                                (p.dead
                                  ? 'bg-destructive'
                                  : p.disabled
                                    ? 'bg-yellow-500'
                                    : 'bg-green-500 shadow-[0_0_5px_1px_rgba(34,197,94,0.6)]')
                              }
                              title={p.dead ? '离线' : p.disabled ? '已禁用' : '在线'}
                            />
                            <div className="min-w-0 flex-1">
                              <div className="flex items-center gap-1.5">
                                <span className="truncate font-mono font-medium" title={p.url}>
                                  {stripCreds(p.url)}
                                </span>
                                {p.dead ? (
                                  <Badge variant="destructive" className="shrink-0">
                                    离线
                                  </Badge>
                                ) : (
                                  <Badge variant="success" className="shrink-0">
                                    在线
                                  </Badge>
                                )}
                                {p.disabled && (
                                  <Badge variant="secondary" className="shrink-0">
                                    已禁用
                                  </Badge>
                                )}
                              </div>
                              <div className="mt-1 flex flex-wrap items-center gap-x-3 gap-y-0.5 text-2xs text-muted-foreground">
                                {p.country && (
                                  <span>出口 {countryFlag(p.country)} {p.country}</span>
                                )}
                                <span>绑定 {p.boundCredentials} 号</span>
                                <span>并发 {p.maxConcurrency}</span>
                                <span>可用 {p.availablePermits}</span>
                                {p.consecutiveFailures > 0 && (
                                  <span className="text-destructive">
                                    连续失败 {p.consecutiveFailures}
                                  </span>
                                )}
                                {p.lastError && (
                                  <Tooltip>
                                    <TooltipTrigger asChild>
                                      <span className="cursor-default text-destructive underline decoration-dotted">
                                        最近错误
                                      </span>
                                    </TooltipTrigger>
                                    <TooltipContent side="top">{p.lastError}</TooltipContent>
                                  </Tooltip>
                                )}
                              </div>
                            </div>
                            <div className="flex shrink-0 items-center gap-1">
                              <Button
                                size="sm"
                                variant="ghost"
                                className="h-7 px-2"
                                onClick={() => handleTest(p.id)}
                                disabled={testingId === p.id}
                                title="测试连通性"
                              >
                                {testingId === p.id ? (
                                  <Loader2 className="h-3.5 w-3.5 animate-spin" />
                                ) : (
                                  <Wifi className="h-3.5 w-3.5" />
                                )}
                              </Button>
                              <Button
                                size="sm"
                                variant="ghost"
                                className="h-7 px-2"
                                onClick={() => startEdit(p)}
                                title="编辑"
                              >
                                <Pencil className="h-3.5 w-3.5" />
                              </Button>
                              <Button
                                size="sm"
                                variant="ghost"
                                className="h-7 px-2 text-destructive hover:text-destructive"
                                onClick={() => setDeleteTarget(p)}
                                title="删除"
                              >
                                <Trash2 className="h-3.5 w-3.5" />
                              </Button>
                            </div>
                          </div>
                          )
                        })}
                      </div>
                    )}
                  </div>
                )
              })}
            </div>
          )}
        </div>

        {/* === 新增/编辑表单 === */}
        <div className="space-y-2 rounded-md border bg-background p-3">
          <div className="flex items-center justify-between">
            <h3 className="text-sm font-medium">
              {editingId != null ? `编辑代理 #${editingId}` : '新增代理'}
            </h3>
            {editingId != null && (
              <Button
                size="sm"
                variant="ghost"
                className="h-7 px-2 text-xs"
                onClick={cancelEdit}
              >
                <X className="mr-1 h-3.5 w-3.5" /> 取消编辑
              </Button>
            )}
          </div>
          <div className="grid grid-cols-1 gap-2 sm:grid-cols-2">
            <div className="sm:col-span-2">
              <label className="mb-1 block text-xs text-muted-foreground">
                代理 URL
              </label>
              <Input
                value={form.url}
                onChange={(e) => setForm({ ...form, url: e.target.value })}
                onBlur={(e) => {
                  const fixed = ensureProxyScheme(e.target.value)
                  if (fixed !== form.url) setForm((f) => ({ ...f, url: fixed }))
                }}
                placeholder="host:port 或 socks5://host:port（离开输入框自动补前缀）"
                className="h-8 text-sm"
              />
            </div>
            <div>
              <label className="mb-1 block text-xs text-muted-foreground">
                用户名（可选）
              </label>
              <Input
                value={form.username}
                onChange={(e) => setForm({ ...form, username: e.target.value })}
                className="h-8 text-sm"
              />
            </div>
            <div>
              <label className="mb-1 block text-xs text-muted-foreground">
                密码（可选{editingId != null ? '，留空不修改' : ''}）
              </label>
              <Input
                type="password"
                value={form.password}
                onChange={(e) => setForm({ ...form, password: e.target.value })}
                className="h-8 text-sm"
              />
            </div>
            <div>
              <label className="mb-1 block text-xs text-muted-foreground">
                代理分组（可选）
              </label>
              <Input
                value={form.region}
                onChange={(e) => setForm({ ...form, region: e.target.value })}
                placeholder="如 US:California"
                className="h-8 text-sm"
              />
            </div>
            <div>
              <label className="mb-1 block text-xs text-muted-foreground">
                最大并发（可选）
              </label>
              <Input
                type="number"
                min="1"
                value={form.maxConcurrency}
                onChange={(e) =>
                  setForm({ ...form, maxConcurrency: e.target.value })
                }
                placeholder="默认"
                className="h-8 text-sm"
              />
            </div>
            <div className="sm:col-span-2">
              <label className="mb-1 block text-xs text-muted-foreground">
                备注（可选）
              </label>
              <Input
                value={form.note}
                onChange={(e) => setForm({ ...form, note: e.target.value })}
                className="h-8 text-sm"
              />
            </div>
          </div>
          <div className="flex items-center justify-between pt-1">
            <label className="flex items-center gap-2 text-sm">
              <Switch
                checked={form.disabled}
                onCheckedChange={(v) => setForm({ ...form, disabled: v })}
              />
              禁用此代理
            </label>
            <Button
              size="sm"
              className="h-8"
              onClick={handleSubmitForm}
              disabled={formPending}
            >
              {formPending ? (
                <Loader2 className="mr-1 h-3.5 w-3.5 animate-spin" />
              ) : (
                <Plus className="mr-1 h-3.5 w-3.5" />
              )}
              {editingId != null ? '保存修改' : '新增代理'}
            </Button>
          </div>
        </div>

        {/* === 批量导入 === */}
        <div className="space-y-2 rounded-md border bg-background p-3">
          <h3 className="text-sm font-medium">批量导入</h3>
          <p className="text-2xs text-muted-foreground">
            每行一个，支持 <code>ip:port:user:pass</code>（默认 socks5）或{' '}
            <code>url</code> / <code>url,user,pass</code>。导入后自动探测出口区域/国家。
          </p>
          <textarea
            value={importText}
            onChange={(e) => setImportText(e.target.value)}
            onBlur={(e) => {
              const fixed = ensureProxySchemeBlock(e.target.value)
              if (fixed !== importText) setImportText(fixed)
            }}
            rows={5}
            placeholder={'38.111.61.59:443:user:pass\nsocks5://1.2.3.4:1080\nhttp://5.6.7.8:8080,user,pass'}
            className="w-full rounded-md border border-input bg-background px-3 py-2 font-mono text-xs"
          />
          <div className="grid grid-cols-1 gap-2 sm:grid-cols-2">
            <div>
              <label className="mb-1 block text-xs text-muted-foreground">
                统一分组（可选）
              </label>
              <Input
                value={importRegion}
                onChange={(e) => setImportRegion(e.target.value)}
                placeholder="如 US:California"
                className="h-8 text-sm"
              />
            </div>
            <div>
              <label className="mb-1 block text-xs text-muted-foreground">
                统一最大并发（可选）
              </label>
              <Input
                type="number"
                min="1"
                value={importMaxConcurrency}
                onChange={(e) => setImportMaxConcurrency(e.target.value)}
                placeholder="默认"
                className="h-8 text-sm"
              />
            </div>
          </div>
          <div className="flex justify-end">
            <Button
              size="sm"
              variant="outline"
              className="h-8"
              onClick={handleImport}
              disabled={importProxies.isPending || !importText.trim()}
            >
              {importProxies.isPending && (
                <Loader2 className="mr-1 h-3.5 w-3.5 animate-spin" />
              )}
              导入
            </Button>
          </div>
        </div>
      </DialogContent>

      {/* 删除确认 */}
      <Dialog
        open={deleteTarget != null}
        onOpenChange={(o) => !o && setDeleteTarget(null)}
      >
        <DialogContent>
          <DialogHeader>
            <DialogTitle>确认删除代理</DialogTitle>
            <DialogDescription>
              删除代理 {deleteTarget ? stripCreds(deleteTarget.url) : ''} 将自动解绑其关联的{' '}
              {deleteTarget?.boundCredentials ?? 0} 个凭据。此操作无法撤销。
            </DialogDescription>
          </DialogHeader>
          <div className="flex justify-end gap-2">
            <Button
              variant="outline"
              onClick={() => setDeleteTarget(null)}
              disabled={deleteProxy.isPending}
            >
              取消
            </Button>
            <Button
              variant="destructive"
              onClick={handleDelete}
              disabled={deleteProxy.isPending}
            >
              {deleteProxy.isPending && (
                <Loader2 className="mr-1 h-3.5 w-3.5 animate-spin" />
              )}
              确认删除
            </Button>
          </div>
        </DialogContent>
      </Dialog>
    </Dialog>
  )
}
