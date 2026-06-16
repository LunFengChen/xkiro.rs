import { useEffect, useState } from 'react'
import { toast } from 'sonner'
import { Loader2, Plus, Trash2, Wifi, Pencil, X, Wand2 } from 'lucide-react'
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
            <div className="space-y-1.5">
              {proxies.map((p) => (
                <div
                  key={p.id}
                  className="flex items-center gap-3 rounded-md border bg-muted/20 px-3 py-2 text-xs"
                >
                  <div className="min-w-0 flex-1">
                    <div className="flex items-center gap-1.5">
                      <span className="truncate font-mono font-medium" title={p.url}>
                        {p.url}
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
                      {(p.region || p.country) && (
                        <span>
                          出口 {[p.region, p.country].filter(Boolean).join(', ')}
                        </span>
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
              ))}
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
                placeholder="http://host:port 或 socks5://host:port"
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
                区域（可选）
              </label>
              <Input
                value={form.region}
                onChange={(e) => setForm({ ...form, region: e.target.value })}
                placeholder="如 us-east-1"
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
            rows={5}
            placeholder={'38.111.61.59:443:user:pass\nsocks5://1.2.3.4:1080\nhttp://5.6.7.8:8080,user,pass'}
            className="w-full rounded-md border border-input bg-background px-3 py-2 font-mono text-xs"
          />
          <div className="grid grid-cols-1 gap-2 sm:grid-cols-2">
            <div>
              <label className="mb-1 block text-xs text-muted-foreground">
                统一区域（可选）
              </label>
              <Input
                value={importRegion}
                onChange={(e) => setImportRegion(e.target.value)}
                placeholder="如 us-east-1"
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
              删除代理 {deleteTarget?.url} 将自动解绑其关联的{' '}
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
