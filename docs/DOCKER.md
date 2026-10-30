# 本仓库 Docker 镜像

镜像地址为 `ghcr.io/sliencio/ting-reader`，提供 `linux/amd64` 和 `linux/arm64`，Docker 会按主机架构选择对应镜像。`2.0.7` 等版本标签固定对应同名 Git 标签的源码，禁止覆盖；`latest` 跟随较新的已发布版本，补发旧版本不会将其回退。

首次成功发布后，仓库所有者需要在 [容器包设置](https://github.com/users/sliencio/packages/container/ting-reader/settings) 中将可见性设置为 Public，之后用户即可免登录拉取。发布使用 GitHub Actions 自带的 `GITHUB_TOKEN`，不需要配置 Docker Hub 密钥。

## 安装与更新

将项目中的 `docker-compose.ghcr.yml` 放在部署目录，执行：

```bash
docker compose -f docker-compose.ghcr.yml pull
docker compose -f docker-compose.ghcr.yml up -d
```

默认使用固定版本 `2.0.7`，访问 `http://服务器地址:3000`。数据、插件、缓存和有声书分别保存在部署目录的 `data`、`plugins`、`temp`、`storage` 中。升级现有实例时继续使用原有数据目录和媒体挂载路径，先备份数据库。

更新至后续版本时，可修改 Compose 文件中的默认版本，或在同目录的 `.env` 中指定：

```dotenv
TING_READER_VERSION=2.0.8
TING_READER_PORT=3000
```

上面是后续版本的配置示例，需等对应镜像发布后使用。随后重新执行 `pull` 和 `up -d`。如需挂载其他媒体目录，可在 Compose 的 `volumes` 中追加原有挂载。

也可单独拉取指定版本：

```bash
docker pull ghcr.io/sliencio/ting-reader:2.0.7
```

## 发布流程

- 推送新版本 Git 标签时，原生 FPK 和 Docker 分别构建；Docker 不会覆盖已有 Release 或附件。
- 在 `Docker Build and Publish` 工作流中手动填写现有版本标签，可补发该版本尚不存在的镜像。工作流始终检出该标签的源码，并检查版本号和完整提交号。
- 两种架构构建成功后才合并发布版本镜像。已有版本镜像会被拒绝覆盖；重建已发布的版本需要使用新版本、新标签。
- Docker 版 FPK 的 Compose 资源已改为引用本仓库 GHCR 镜像；原生 FPK 的应用标识、数据目录不变。
