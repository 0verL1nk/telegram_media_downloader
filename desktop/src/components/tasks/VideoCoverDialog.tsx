import { useEffect, useState } from "react";
import { api, friendlyError, type CoverSaveResult, type DownloadTask, type VideoPreviewInfo } from "../../lib/api";
import { Button } from "../ui/button";
import { AlertDialog, AlertDialogContent, AlertDialogDescription, AlertDialogTitle } from "../ui/alert-dialog";

function timeLabel(seconds: number) {
  const safe = Math.max(0, Math.floor(seconds));
  return `${Math.floor(safe / 60)}:${String(safe % 60).padStart(2, "0")}`;
}

export function VideoCoverDialog({
  task,
  onClose,
  onSaved,
}: {
  task: DownloadTask | null;
  onClose: () => void;
  onSaved: (result: CoverSaveResult) => void;
}) {
  const [video, setVideo] = useState<VideoPreviewInfo | null>(null);
  const [timestamp, setTimestamp] = useState(0);
  const [frame, setFrame] = useState("");
  const [loading, setLoading] = useState(false);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState("");

  useEffect(() => {
    if (!task) return;
    let disposed = false;
    setVideo(null);
    setFrame("");
    setTimestamp(0);
    setError("");
    setLoading(true);
    void api.probeTaskVideo(task.taskId).then((info) => {
      if (!disposed) setVideo(info);
    }).catch((reason) => {
      if (!disposed) setError(friendlyError(reason));
    }).finally(() => {
      if (!disposed) setLoading(false);
    });
    return () => { disposed = true; };
  }, [task?.taskId]);

  useEffect(() => {
    if (!task || !video) return;
    let disposed = false;
    const timer = window.setTimeout(() => {
      setLoading(true);
      void api.extractVideoFrame(task.taskId, timestamp).then((jpeg) => {
        if (!disposed) setFrame(`data:image/jpeg;base64,${jpeg}`);
      }).catch((reason) => {
        if (!disposed) setError(friendlyError(reason));
      }).finally(() => {
        if (!disposed) setLoading(false);
      });
    }, 180);
    return () => {
      disposed = true;
      window.clearTimeout(timer);
    };
  }, [task?.taskId, video, timestamp]);

  async function saveCover() {
    if (!task || !frame || saving) return;
    setSaving(true);
    setError("");
    try {
      const jpeg = frame.slice(frame.indexOf(",") + 1);
      const result = await api.setTaskVideoCover(task.taskId, jpeg);
      onSaved(result);
      onClose();
    } catch (reason) {
      setError(friendlyError(reason));
    } finally {
      setSaving(false);
    }
  }

  const duration = video?.durationSeconds ?? 0;
  return (
    <AlertDialog open={task !== null} onOpenChange={(open) => { if (!open && !saving) onClose(); }}>
      <AlertDialogContent className="video-cover-modal">
        <AlertDialogTitle asChild><h2>选取视频封面</h2></AlertDialogTitle>
        <AlertDialogDescription asChild>
          <p className="video-cover-name" title={task?.fileName ?? ""}>{task?.fileName || "本地视频"}</p>
        </AlertDialogDescription>
        <div className="video-cover-preview">
          {frame ? <img src={frame} alt={`视频 ${timeLabel(timestamp)} 处的画面`} /> : null}
          {loading ? <span className="video-cover-loading">正在读取视频画面…</span> : null}
          {!frame && !loading && !error ? <span className="video-cover-loading">尚未读取到画面</span> : null}
        </div>
        {video ? (
          <div className="video-cover-controls">
            <span className="video-cover-time">{timeLabel(timestamp)}</span>
            <input
              aria-label="选择封面帧位置"
              type="range"
              min={0}
              max={duration}
              step={0.05}
              value={timestamp}
              onChange={(event) => setTimestamp(Number(event.currentTarget.value))}
              disabled={saving || duration <= 0}
            />
            <span className="video-cover-time">{timeLabel(duration)}</span>
          </div>
        ) : null}
        {video?.coverPath ? <small className="video-cover-hint">当前视频已有封面；保存后会替换视频内嵌封面。</small> : null}
        {video && !video.downloadComplete ? <small className="video-cover-hint">视频仍在下载；封面会先保存，下载完成后写入视频文件。</small> : null}
        {error ? <p className="video-cover-error" role="alert">{error}</p> : null}
        <div className="modal-foot">
          <Button variant="ghost" onClick={onClose} disabled={saving}>取消</Button>
          <Button variant="primary" onClick={() => void saveCover()} disabled={!frame || loading || saving}>
            {saving ? "正在写入视频…" : "设为封面"}
          </Button>
        </div>
      </AlertDialogContent>
    </AlertDialog>
  );
}
