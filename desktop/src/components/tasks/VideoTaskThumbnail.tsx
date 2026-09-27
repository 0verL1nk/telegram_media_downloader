import { useEffect, useRef, useState } from "react";
import { api, type DownloadTask } from "../../lib/api";
import { isVideoTask } from "../../lib/format";

const PARTIAL_PREVIEW_BYTES = 1024 * 1024;

export function VideoTaskThumbnail({
  task,
  fallback,
  tone,
  showCover,
}: {
  task: DownloadTask;
  fallback: string;
  tone: string;
  showCover: boolean;
}) {
  const element = useRef<HTMLSpanElement>(null);
  const [visible, setVisible] = useState(false);
  const [thumbnail, setThumbnail] = useState<string | null>(null);
  const video = isVideoTask(task);
  const completed = task.status.toLowerCase() === "completed";
  const partialReady = task.downloadedBytes >= PARTIAL_PREVIEW_BYTES;
  const cachedCover = task.coverPath || task.previewPath;

  useEffect(() => {
    const node = element.current;
    if (!node) return;
    if (!("IntersectionObserver" in window)) {
      setVisible(true);
      return;
    }
    const observer = new IntersectionObserver(([entry]) => {
      if (entry.isIntersecting) {
        setVisible(true);
        observer.disconnect();
      }
    }, { rootMargin: "96px" });
    observer.observe(node);
    return () => observer.disconnect();
  }, []);

  useEffect(() => {
    if (!visible || !showCover || !video || (!completed && !partialReady)) {
      if (!showCover || !video) setThumbnail(null);
      return;
    }
    let active = true;
    void api.getTaskVideoThumbnail(task.taskId).then((path) => {
      if (active) setThumbnail(path);
    }).catch(() => {
      // A partial file may not contain enough container metadata yet; retry after completion.
    });
    return () => { active = false; };
  }, [visible, showCover, video, task.taskId, completed, partialReady, cachedCover]);

  const path = showCover ? thumbnail : null;
  return (
    <span ref={element} className={`st ${tone}${path ? " st-thumb" : ""}`}>
      {path ? <img src={path} alt="视频预览封面" loading="lazy" onError={() => setThumbnail(null)} /> : fallback}
    </span>
  );
}
