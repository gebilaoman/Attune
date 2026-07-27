//! AI 提供者 —— 复用自 DirDetective,大幅精简。
//!
//! 听读工具只用到「即席自由提问」:`ask_raw(prompt)` 发任意 prompt、返回模型原始文本。
//! 清理四步 / 翻译 / 点词即查全部走这条通道(带上下文,解决一词多义/行话)。
//!
//! 兼容所有 OpenAI Chat Completions 协议的服务(智谱 GLM / DeepSeek / OpenRouter / OpenAI …)。

/// OpenAI 兼容的 chat completions 提供者。
pub struct AIProvider {
    api_key: String,
    model: String,
    base_url: String,
    /// 关掉「思考」模式(智谱 GLM 推理模型特有字段 thinking)。
    /// 校对/清理/查词这类交互式小任务不需要深度推理,开着会让单次请求慢到几分钟。
    disable_thinking: bool,
}

impl AIProvider {
    pub fn new(api_key: String) -> Self {
        Self {
            api_key,
            model: "glm-5.2".to_string(),
            base_url: "https://open.bigmodel.cn/api/paas/v4".to_string(),
            disable_thinking: false,
        }
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into().trim_end_matches('/').to_string();
        self
    }

    /// 关掉思考模式(仅对支持 thinking 字段的智谱 GLM 有效;其他厂商别开,避免 400)。
    pub fn with_thinking_disabled(mut self, disabled: bool) -> Self {
        self.disable_thinking = disabled;
        self
    }

    /// 即席自由提问:发送任意 prompt,返回模型原始文本(不解析、不缓存)。
    /// 清理 / 翻译 / 查词都走这里。
    pub async fn ask_raw(&self, prompt: &str) -> Result<String, String> {
        self.call_api_raw(prompt).await
    }

    /// 发一次 chat completions 请求,抽出 `choices[0].message.content`。
    async fn call_api_raw(&self, prompt: &str) -> Result<String, String> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(180))
            .build()
            .map_err(|e| format!("创建 HTTP 客户端失败: {}", e))?;

        let mut request_body = serde_json::json!({
            "model": self.model,
            "messages": [{ "role": "user", "content": prompt }],
            "temperature": 0.3,
            "top_p": 0.7,
        });
        // 智谱 GLM 推理模型:关掉思考,交互式任务从几分钟降到 1~2 秒。
        if self.disable_thinking {
            request_body["thinking"] = serde_json::json!({ "type": "disabled" });
        }

        let response = client
            .post(format!("{}/chat/completions", self.base_url))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&request_body)
            .send()
            .await
            .map_err(|e| format!("API 请求失败: {}", e))?;

        if !response.status().is_success() {
            let status = response.status();
            let error_text = response
                .text()
                .await
                .unwrap_or_else(|_| "无法读取错误响应".to_string());
            return Err(format!("API 返回错误 {}: {}", status, error_text));
        }

        let response_json: serde_json::Value = response
            .json()
            .await
            .map_err(|e| format!("解析响应失败: {}", e))?;

        let content = response_json["choices"][0]["message"]["content"]
            .as_str()
            .ok_or("响应格式错误: 缺少 content")?;

        Ok(content.to_string())
    }
}

/// 去掉模型回复里常见的 ```json / ``` 代码块包裹。
pub fn strip_code_fence(content: &str) -> &str {
    let content = content.trim();
    let content = if let Some(rest) = content.strip_prefix("```json") {
        rest.trim()
    } else if let Some(rest) = content.strip_prefix("```") {
        rest.trim()
    } else {
        content
    };
    if let Some(rest) = content.strip_suffix("```") {
        rest.trim()
    } else {
        content
    }
}

#[cfg(test)]
mod tests {
    use super::strip_code_fence;

    #[test]
    fn strips_json_fence() {
        assert_eq!(strip_code_fence("```json\n{\"a\":1}\n```"), "{\"a\":1}");
        assert_eq!(strip_code_fence("```\n{}\n```"), "{}");
        assert_eq!(strip_code_fence("{\"a\":1}"), "{\"a\":1}");
    }
}
