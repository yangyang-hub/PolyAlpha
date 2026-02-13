// PolyAlpha Jenkins CI/CD Pipeline
//
// 适用于 Docker 部署的 Jenkins（挂载宿主机 Docker Socket）
// PostgreSQL / Prometheus / Grafana 均为独立部署，不在 docker-compose 中管理
//
// ======================= 前置要求 =======================
//
// 1. Jenkins 容器启动时挂载 Docker Socket + 安装 docker CLI:
//
//    docker run -d --name jenkins \
//      --restart unless-stopped \
//      -p 8888:8080 -p 50000:50000 \
//      -v jenkins_home:/var/jenkins_home \
//      -v /var/run/docker.sock:/var/run/docker.sock \
//      -v /usr/bin/docker:/usr/bin/docker \
//      --group-add $(getent group docker | cut -d: -f3) \
//      --network host \
//      jenkins/jenkins:lts
//
//    说明:
//      - /var/run/docker.sock  : 让 Jenkins 能调用宿主机 Docker
//      - /usr/bin/docker       : 共享宿主机 docker CLI
//      - --group-add           : 添加 docker 组权限
//      - --network host        : 使用宿主机网络（bot 也用 host 网络，健康检查直接 localhost）
//
// 2. Jenkins 安装插件: Pipeline, Git, Docker Pipeline
//
// 3. Jenkins 凭据中添加:
//    - 'polyalpha-env' : Secret file 类型，上传 .env 文件（含 POLYMARKET_PRIVATE_KEY 等）
//
// 4. 新建 Pipeline 任务:
//    a. Jenkins 首页 → New Item → Pipeline
//    b. Pipeline 区域 → Definition: Pipeline script（直接粘贴本文件内容）
//    c. 保存后点击 Build Now 即可
//
// ========================================================

pipeline {
    agent any

    environment {
        PROJECT        = 'polyalpha'
        BOT_CONTAINER  = 'polyalpha-bot'

        // ====== 修改这里 ======
        GIT_REPO       = 'https://github.com/yangyang-hub/PolyAlpha.git'
        GIT_BRANCH     = 'master'
        // =====================

        // 镜像 tag: 分支-构建号（commit hash 在 Checkout 阶段追加）
        IMAGE_TAG      = "${GIT_BRANCH}-${env.BUILD_NUMBER}"
    }

    options {
        timeout(time: 30, unit: 'MINUTES')
        disableConcurrentBuilds()
        buildDiscarder(logRotator(numToKeepStr: '10'))
        timestamps()
    }

    stages {

        // ============================================================
        // Stage 1: 从 GitHub 拉取代码
        // ============================================================
        stage('Checkout') {
            steps {
                git branch: env.GIT_BRANCH, url: env.GIT_REPO
                sh 'git log --oneline -5'
                script {
                    env.GIT_SHORT = sh(script: 'git rev-parse --short HEAD', returnStdout: true).trim()
                    env.IMAGE_TAG = "${env.IMAGE_TAG}-${env.GIT_SHORT}"
                }
                echo "Image tag: ${env.IMAGE_TAG}"
            }
        }

        // ============================================================
        // Stage 2: 测试（在临时 Rust 容器中执行）
        // ============================================================
        stage('Test') {
            steps {
                sh """
                    docker build \
                        -f docker/Dockerfile \
                        --target test \
                        -t ${PROJECT}-test:${IMAGE_TAG} \
                        .
                """
                echo "Tests passed"
            }
        }

        // ============================================================
        // Stage 3: 构建镜像
        // ============================================================
        stage('Build Image') {
            steps {
                sh """
                    docker build \
                        -f docker/Dockerfile \
                        -t ${PROJECT}:${IMAGE_TAG} \
                        -t ${PROJECT}:latest \
                        .
                """
                echo "Built ${PROJECT}:${IMAGE_TAG}"
            }
        }

        // ============================================================
        // Stage 4: 部署
        // PostgreSQL / Prometheus / Grafana 均已独立部署
        // docker-compose 仅管理 polyalpha bot 容器
        // ============================================================
        stage('Deploy') {
            steps {
                withCredentials([file(credentialsId: 'polyalpha-env', variable: 'ENV_FILE')]) {
                    sh """
                        # 停止并移除旧容器
                        docker stop ${BOT_CONTAINER} 2>/dev/null || true
                        docker rm ${BOT_CONTAINER} 2>/dev/null || true

                        # 启动新容器（host 网络，直接访问宿主机 PostgreSQL/Prometheus）
                        docker run -d \
                            --name ${BOT_CONTAINER} \
                            --restart unless-stopped \
                            --network host \
                            --env-file "\$ENV_FILE" \
                            -e RUN_MODE=production \
                            ${PROJECT}:${IMAGE_TAG}

                        # 清理悬空镜像
                        docker image prune -f 2>/dev/null || true
                    """
                }
            }
        }

        // ============================================================
        // Stage 5: 健康检查
        // bot 使用 network_mode: host，直接通过 localhost:18381 访问
        // ============================================================
        stage('Health Check') {
            steps {
                sh """
                    echo "Waiting for bot to start..."
                    sleep 8

                    # 检查容器运行状态
                    if ! docker ps --filter "name=${BOT_CONTAINER}" --filter "status=running" -q | grep -q .; then
                        echo "ERROR: ${BOT_CONTAINER} is not running"
                        docker logs ${BOT_CONTAINER} --tail 50 2>/dev/null || true
                        exit 1
                    fi

                    # 健康端点探测（bot 用 host 网络，直接 localhost）
                    HEALTH_URL="http://localhost:18381/health"

                    for i in \$(seq 1 12); do
                        HTTP_CODE=\$(curl -sf -o /dev/null -w "%{http_code}" "\$HEALTH_URL" 2>/dev/null || echo "000")
                        if [ "\$HTTP_CODE" = "200" ]; then
                            echo "Health check PASSED (HTTP 200)"
                            curl -sf "\$HEALTH_URL" 2>/dev/null || true
                            exit 0
                        fi
                        echo "Attempt \$i/12: HTTP \$HTTP_CODE, retrying in 5s..."
                        sleep 5
                    done

                    echo "ERROR: Health check failed after 60s"
                    docker logs ${BOT_CONTAINER} --tail 100 2>/dev/null || true
                    exit 1
                """
            }
        }
    }

    // ================================================================
    // Post
    // ================================================================
    post {
        success {
            echo "SUCCESS: ${PROJECT}:${env.IMAGE_TAG} deployed"
        }
        failure {
            echo "FAILED — dumping bot logs:"
            sh "docker logs ${BOT_CONTAINER} --tail 100 2>/dev/null || true"
        }
        always {
            sh 'rm -f .env'
            cleanWs()
        }
    }
}
