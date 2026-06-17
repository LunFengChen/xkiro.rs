import { useState, useMemo } from 'react'
import { toast } from 'sonner'
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogFooter,
} from '@/components/ui/dialog'
import { Button } from '@/components/ui/button'
import { startImportJob } from '@/api/credentials'
import { extractErrorMessage } from '@/lib/utils'

interface KamImportDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  /** 后台任务已启动后回调，携带 jobId 和总数，由 dashboard 维护浮窗状态 */
  onJobStart?: (jobId: string, total: number) => void
}

// KAM 导出 JSON 中的账号结构
interface KamAccount {
  email?: string
  userId?: string | null
  nickname?: string
  credentials: {
    refreshToken: string
    clientId?: string
    clientSecret?: string
    region?: string
    authMethod?: string
    startUrl?: string
  }
  machineId?: string
  status?: string
}

// 兼容 KAM 1.8.3 新版平铺格式，统一转换为旧格式（credentials 嵌套结构）
function normalizeKamAccount(item: unknown): unknown {
  if (typeof item !== 'object' || item === null) return item
  const obj = item as Record<string, unknown>
  // 新格式：refreshToken 直接在账号对象上，无 credentials 嵌套
  if (typeof obj.refreshToken === 'string' && typeof obj.credentials === 'undefined') {
    const email = typeof obj.email === 'string' ? obj.email : undefined
    const userId =
      typeof obj.userId === 'string' || obj.userId === null ? (obj.userId as string | null) : undefined
    const nickname =
      typeof obj.nickname === 'string'
        ? obj.nickname
        : typeof obj.label === 'string'
          ? (obj.label as string)
          : undefined
    const status = typeof obj.status === 'string' ? obj.status : undefined
    const machineId = typeof obj.machineId === 'string' ? obj.machineId : undefined
    const clientId = typeof obj.clientId === 'string' ? obj.clientId : undefined
    const clientSecret = typeof obj.clientSecret === 'string' ? obj.clientSecret : undefined
    const region = typeof obj.region === 'string' ? obj.region : undefined
    const authMethod = typeof obj.authMethod === 'string' ? obj.authMethod : undefined
    const startUrl = typeof obj.startUrl === 'string' ? obj.startUrl : undefined

    return {
      email,
      userId,
      nickname,
      status,
      machineId,
      credentials: {
        refreshToken: obj.refreshToken,
        clientId,
        clientSecret,
        region,
        authMethod,
        startUrl,
      },
    }
  }
  return item
}

// 校验元素是否为有效的 KAM 账号结构
function isValidKamAccount(item: unknown): item is KamAccount {
  if (typeof item !== 'object' || item === null) return false
  const obj = item as Record<string, unknown>
  if (typeof obj.credentials !== 'object' || obj.credentials === null) return false
  const cred = obj.credentials as Record<string, unknown>
  return typeof cred.refreshToken === 'string' && cred.refreshToken.trim().length > 0
}

// 解析 KAM 导出 JSON，支持单账号和多账号格式
function parseKamJson(raw: string): KamAccount[] {
  const parsed = JSON.parse(raw)

  let rawItems: unknown[]

  // 标准 KAM 导出格式：{ version, accounts: [...] }
  if (parsed.accounts && Array.isArray(parsed.accounts)) {
    rawItems = parsed.accounts
  } else if (Array.isArray(parsed)) {
    rawItems = parsed
  } else if (typeof parsed === 'object' && parsed !== null) {
    rawItems = [parsed]
  } else {
    throw new Error('不支持的格式：需要对象数组或 {accounts:[...]} 格式')
  }

  const normalized = rawItems.map(normalizeKamAccount)
  const valid = normalized.filter(isValidKamAccount)

  if (valid.length === 0) {
    throw new Error('未找到包含有效 refreshToken 的账号')
  }

  return valid
}

export function KamImportDialog({ open, onOpenChange, onJobStart }: KamImportDialogProps) {
  const [jsonInput, setJsonInput] = useState('')
  const [submitting, setSubmitting] = useState(false)
  const [skipErrorAccounts, setSkipErrorAccounts] = useState(true)

  const resetForm = () => {
    setJsonInput('')
    setSubmitting(false)
  }

  const { previewAccounts, parseError } = useMemo(() => {
    if (!jsonInput.trim()) return { previewAccounts: [] as KamAccount[], parseError: '' }
    try {
      return { previewAccounts: parseKamJson(jsonInput), parseError: '' }
    } catch (e) {
      return { previewAccounts: [] as KamAccount[], parseError: extractErrorMessage(e) }
    }
  }, [jsonInput])

  const errorAccountCount = previewAccounts.filter(a => a.status === 'error').length

  const handleImport = async () => {
    let validAccounts: KamAccount[]
    try {
      const accounts = parseKamJson(jsonInput)
      if (accounts.length === 0) {
        toast.error('没有可导入的账号')
        return
      }
      validAccounts = skipErrorAccounts
        ? accounts.filter(a => a.status !== 'error')
        : accounts
      if (validAccounts.length === 0) {
        toast.error('过滤后没有可导入的账号')
        return
      }
    } catch (error) {
      toast.error('JSON 格式错误: ' + extractErrorMessage(error))
      return
    }

    // 构建后端 token.json 格式
    const items = validAccounts.map(a => ({
      refreshToken: a.credentials.refreshToken,
      clientId: a.credentials.clientId,
      clientSecret: a.credentials.clientSecret,
      region: a.credentials.region,
      authMethod: a.credentials.authMethod,
      machineId: a.machineId,
    }))

    try {
      setSubmitting(true)
      const { jobId, total } = await startImportJob(items, false)
      toast.success(`已提交 ${total} 个账号，后台导入中...`)
      onJobStart?.(jobId, total)
      onOpenChange(false)
      resetForm()
    } catch (error) {
      toast.error('提交失败: ' + extractErrorMessage(error))
    } finally {
      setSubmitting(false)
    }
  }

  return (
    <Dialog
      open={open}
      onOpenChange={(newOpen) => {
        if (!newOpen && submitting) return
        if (!newOpen) resetForm()
        onOpenChange(newOpen)
      }}
    >
      <DialogContent className="sm:max-w-2xl max-h-[80vh] flex flex-col">
        <DialogHeader>
          <DialogTitle>KAM 账号导入</DialogTitle>
        </DialogHeader>

        <div className="flex-1 overflow-y-auto space-y-4 py-4">
          <div className="space-y-2">
            <label className="text-sm font-medium">KAM 导出 JSON</label>
            <div
              onDragOver={(e) => { e.preventDefault(); e.stopPropagation() }}
              onDrop={async (e) => {
                e.preventDefault(); e.stopPropagation()
                if (submitting) return
                const file = e.dataTransfer.files?.[0]
                if (!file) return
                if (!/\.(json|txt)$/i.test(file.name) && file.type && !file.type.includes('json') && !file.type.includes('text')) {
                  toast.error('仅支持 .json / 文本文件')
                  return
                }
                try {
                  const text = await file.text()
                  setJsonInput(text)
                  toast.success(`已读取 ${file.name}`)
                } catch (err) {
                  toast.error('读取文件失败：' + extractErrorMessage(err))
                }
              }}
            >
              <textarea
                placeholder={'粘贴或拖入 Kiro Account Manager 导出的 JSON\n\n支持 KAM 1.8.3+ 新版平铺格式：\n[\n  {\n    "email": "...",\n    "refreshToken": "...",\n    "clientId": "...",\n    "clientSecret": "...",\n    "region": "us-east-1"\n  }\n]\n\n也支持旧版嵌套格式：\n{\n  "version": "1.5.0",\n  "accounts": [\n    {\n      "email": "...",\n      "credentials": {\n        "refreshToken": "...",\n        "clientId": "...",\n        "clientSecret": "...",\n        "region": "us-east-1"\n      }\n    }\n  ]\n}'}
                value={jsonInput}
                onChange={(e) => setJsonInput(e.target.value)}
                disabled={submitting}
                className="flex min-h-[200px] w-full rounded-md border border-input bg-background px-3 py-2 text-sm ring-offset-background placeholder:text-muted-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 disabled:cursor-not-allowed disabled:opacity-50 font-mono"
              />
            </div>
            <p className="text-xs text-muted-foreground">💡 拖入 .json 文件可直接读取 · 提交后后台处理，可关闭此窗口</p>
          </div>

          {/* 解析预览 */}
          {parseError && (
            <div className="text-sm text-red-600 dark:text-red-400">解析失败: {parseError}</div>
          )}
          {previewAccounts.length > 0 && (
            <div className="space-y-2">
              <div className="text-sm text-muted-foreground">
                识别到 {previewAccounts.length} 个账号
                {errorAccountCount > 0 && `（其中 ${errorAccountCount} 个为 error 状态）`}
              </div>
              {errorAccountCount > 0 && (
                <label className="flex items-center gap-2 text-sm">
                  <input
                    type="checkbox"
                    checked={skipErrorAccounts}
                    onChange={(e) => setSkipErrorAccounts(e.target.checked)}
                    className="rounded border-gray-300"
                  />
                  跳过 error 状态的账号
                </label>
              )}
            </div>
          )}
        </div>

        <DialogFooter>
          <Button
            type="button"
            variant="outline"
            onClick={() => { onOpenChange(false); resetForm() }}
            disabled={submitting}
          >
            取消
          </Button>
          <Button
            type="button"
            onClick={handleImport}
            disabled={submitting || !jsonInput.trim() || previewAccounts.length === 0 || !!parseError}
          >
            {submitting ? '提交中...' : '后台导入'}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
