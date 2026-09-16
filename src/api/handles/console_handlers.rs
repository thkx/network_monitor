// 控制台页面处理器：单文件前端构建期打包进二进制，部署零外部资源
use actix_web::HttpResponse;

// GET /：返回内嵌的Web控制台（原生HTML/JS，无构建链）
pub async fn get_console() -> HttpResponse {
    HttpResponse::Ok()
        .content_type("text/html; charset=utf-8")
        .body(include_str!("../../../static/index.html"))
}
