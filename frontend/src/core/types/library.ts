export interface ScraperConfig {
  default_sources?: string[];
  cover_sources?: string[];
  intro_sources?: string[];
  author_sources?: string[];
  narrator_sources?: string[];
  tags_sources?: string[];
  nfo_writing_enabled?: boolean;
  metadata_writing_enabled?: boolean;
  metadata_priority?: string[];
  extract_audio_cover?: boolean;
  extract_extra_chapters?: boolean;
  auto_generate_series?: boolean;
  disable_watcher?: boolean;
  cloud_mode?: boolean;
  scheduled_sync_enabled?: boolean;
  scheduled_sync_interval?: 'hourly' | 'daily' | 'weekly' | 'monthly';
  book_name_layout?:
    | 'auto'
    | 'title_author_narrator'
    | 'title_narrator_author'
    | 'title_author'
    | 'title_narrator'
    | 'title_only'
    | 'custom';
  book_name_regex?: string;
}

export interface Library {
  id: string;
  name: string;
  library_type: 'webdav' | 'local' | 'rss';
  url: string;
  username?: string;
  password?: string;
  root_path: string;
  last_scanned_at?: string;
  scraper_config?: ScraperConfig;
  created_at: string;
}
