# 阅读状态、历史与书签

所有接口需要登录，数据属于当前用户。书籍列表和书籍详情的 `progress_percent` 为 0–100：未播放为 0，全部章节播放完成或手动标记已读为 100。隐藏历史不改变进度。

## POST /api/books/read-status

```json
{"book_ids":["book-id"],"read":true}
```

每次接受 1–200 本有访问权限的书籍，成功返回 `204 No Content`。

- `read: true` 标记已读，不生成播放历史或收听时长。
- `read: false` 标记未读，清除这些书籍的全部章节进度（含隐藏历史）和手动已读标记。
- 书签和后台收听统计保留，其他用户的数据不受影响。

## POST /api/books/:bookId/chapters/read-status

```json
{"chapter_ids":["chapter-id"],"read":true}
```

每批接受 1–200 个属于该书的章节，需要当前用户具有书籍访问权限。先校验整批，成功后统一写入并返回 `204 No Content`；不存在或不属于该书的章节返回 `400`，整批不写入。

- `read: true` 保存当前账号的独立章节已读标记，支持未知时长章节，重复提交幂等。
- `read: false` 取消这些章节的独立标记，保留实际播放进度；实际已听完的章节仍可按进度显示完成。
- 不修改续播位置、最近收听、播放历史或收听时长。章节已读参与书籍 `progress_percent`，不影响其他用户。
- 两种章节列表响应均增加布尔值 `is_read`，表示独立已读标记；原有 `progress_position`、`progress_updated_at` 保持原意。
- 书籍标记未读、清除指定进度时，一并清除对应章节已读标记；仅隐藏历史时保留。

## 分页约定

以下列表接受 `page`（从 1 开始）和 `page_size`（默认 40，最多 100），响应结构为：

```json
{"items":[],"total":0,"page":1,"page_size":40}
```

列表仅包含当前用户有访问权限的书籍，按最近更新时间倒序排列。

## GET /api/history/books

返回按书籍聚合的可见历史，`items` 中每项包含：

```json
{
  "book_id":"book-id",
  "book_title":"书名",
  "cover_url":null,
  "library_id":"library-id",
  "chapter_count":1500,
  "updated_at":"2026-09-30T00:00:00Z",
  "latest_chapter_id":"chapter-id",
  "latest_chapter_title":"最近收听章节",
  "latest_position":60.0,
  "latest_duration":120.0,
  "latest_note":null
}
```

该接口只附带一条最近章节摘要用于卡片显示，不返回章节列表。客户端展开书籍后再请求章节分页，翻页替换当前列表，避免一次加载或持续积累大量章节。

## GET /api/history/summary

返回可见历史汇总，无需下载章节列表：

```json
{"books":1,"chapters":1500,"position_seconds":75000.0}
```

`position_seconds` 是各章节已保存位置之和，不是独立收听事件累计时长。

## GET /api/history/books/:bookId/chapters

分页返回当前用户该书的可见章节历史，条目结构见 [播放进度](progress.md) 的 `ProgressResponse`。相同更新时间以记录 ID 排序，分页顺序稳定。

清除历史使用 `POST /api/progress/recent/delete`，支持 `all`、`book_ids`、`progress_ids`、`chapter_ids` 和 `clear_progress`，详见 [播放进度](progress.md)。

## GET /api/bookmarks/books

按书籍聚合书签，条目结构与历史书籍相同，`chapter_count` 表示书签数量，`latest_*` 表示最近更新的书签位置、章节及备注。

## GET /api/bookmarks/books/:bookId

分页返回该书的书签：

```json
{
  "items":[{
    "id":"bookmark-id",
    "book_id":"book-id",
    "chapter_id":"chapter-id",
    "chapter_title":"章节名",
    "chapter_duration":120.0,
    "position":12.5,
    "note":"备注",
    "created_at":"2026-09-30T00:00:00Z",
    "updated_at":"2026-09-30T00:00:00Z"
  }],
  "total":1,
  "page":1,
  "page_size":40
}
```

`chapter_duration` 为章节时长（秒），用于计算书签位置对应的章节进度；章节元数据缺少时长时使用当前用户保存的播放时长，仍未知时为 0。

## POST /api/bookmarks

```json
{"book_id":"book-id","chapter_id":"chapter-id","position":12.5,"note":"备注"}
```

`position` 为非负、有限的秒数；章节必须属于书籍。`note` 可省略，最多 2000 个字符。成功返回 `201 Created` 和新书签对象。跳转时使用书签保存的位置。

## PUT /api/bookmarks/:id

```json
{"note":"新备注"}
```

修改当前用户的书签备注并更新排序时间，成功返回 `204 No Content`。

## DELETE /api/bookmarks/:id

删除当前用户的书签，成功返回 `204 No Content`。无法访问其他用户的书签；删除书籍或章节会级联删除对应书签。

## 客户端偏好

`bookshelf_progress_enabled` 在个性化设置中控制封面进度显示。播放倍速范围为 0.5–3.0，客户端按 0.1 调节。音量由客户端本地保存，切换章节继续使用本地音量，不同步为服务器账号音量。
