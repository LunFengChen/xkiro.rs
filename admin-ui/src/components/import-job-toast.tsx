/**
 * KAM 后台导入进度浮窗
 *
 * 用法：
 *   const [importJob, setImportJob] = useState<{jobId: string; total: number} | null>(null)
 *
 *   <KamImportDialog onJobStart={(jobId, total) => setImportJob({ jobId, total })} ... />
 *   {importJob && (
 *     <ImportJobToast jobId={importJob.jobId} total={importJob.total}
 *       onDone={() => setImportJob(null)} />
 *   )}
 */
import { useEffect, useRef, useState } from 'react'
import { CheckCircle2, Loader2, XCircle } from 'lucide-react'
import { useQueryClient } from '@tanstack/react-query'
import { getImportJob, type ImportJobSnapshot } from '@/api/credentials'

interface ImportJobToastProps {
  jobId: string
  total: number
  onDone?: () => void
}

const POLL_INTERVAL_MS = 1500

export function ImportJobToast({ jobId, total, onDone }: ImportJobToastProps) {
  const [snap, setSnap] = useState<ImportJobSnapshot | null>(null)
  const [dismissed, setDismissed] = useState(false)
  const timerRef = useRef<ReturnType<typeof setInterval> | null>(null)
  const queryClient = useQueryClient()

  useEffect(() => {
    const poll = async () => {
      try {
        const data = await getImportJob(jobId)
        setSnap(data)
        if (data.status !== 'Running') {
          if (timerRef.current) clearInterval(timerRef.current)
          // 刷新账号列表
          queryClient.invalidateQueries({ queryKey: ['credentials'] })
        }
      } catch {
        // 查询失败静默重试
      }
    }

    poll()
    timerRef.current = setInterval(poll, POLL_INTERVAL_MS)
    return () => {
      if (timerRef.current) clearInterval(timerRef.current)
    }
  }, [jobId]) // eslint-disable-line react-hooks/exhaustive-deps

  if (dismissed) return null

  const done = snap?.done ?? 0
  const pct = total > 0 ? Math.round((done / total) * 100) : 0
  const isDone = snap?.status === 'Done'
  const isFailed = snap?.status === 'Failed'

  return (
    <div className="fixed bottom-4 right-4 z-50 w-72 rounded-lg border bg-background shadow-lg p-4 space-y-2">
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2 text-sm font-medium">
          {isDone ? (
            <CheckCircle2 className="w-4 h-4 text-green-500" />
          ) : isFailed ? (
            <XCircle className="w-4 h-4 text-red-500" />
          ) : (
            <Loader2 className="w-4 h-4 animate-spin text-blue-500" />
          )}
          {isDone ? '导入完成' : isFailed ? '导入失败' : '后台导入中'}
        </div>
        {(isDone || isFailed) && (
          <button
            onClick={() => { setDismissed(true); onDone?.() }}
            className="text-xs text-muted-foreground hover:text-foreground"
          >
            关闭
          </button>
        )}
      </div>

      {/* 进度条 */}
      <div className="w-full bg-secondary rounded-full h-1.5">
        <div
          className={`h-1.5 rounded-full transition-all ${isDone ? 'bg-green-500' : isFailed ? 'bg-red-500' : 'bg-blue-500'}`}
          style={{ width: `${isDone ? 100 : pct}%` }}
        />
      </div>

      <div className="flex justify-between text-xs text-muted-foreground">
        <span>{done} / {total}</span>
        {snap && (
          <span>
            {snap.added > 0 && <span className="text-green-600 mr-1">+{snap.added}</span>}
            {snap.skipped > 0 && <span className="text-yellow-600 mr-1">跳过{snap.skipped}</span>}
            {snap.invalid > 0 && <span className="text-red-600">失败{snap.invalid}</span>}
          </span>
        )}
      </div>

      {snap?.error && (
        <div className="text-xs text-red-600 truncate">{snap.error}</div>
      )}
    </div>
  )
}
