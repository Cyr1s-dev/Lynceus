//! Module 配置控制面写入。

use models::ModuleConfig;

use crate::errors::EngineError;
use crate::manager::AuditManager;

impl AuditManager {
    /// 校验并创建 Module 配置。
    ///
    /// # Errors
    /// 配置违反模块不变量或仓储写入失败。
    pub fn create_module(&self, module: ModuleConfig) -> Result<ModuleConfig, EngineError> {
        let module = module
            .validated()
            .map_err(|error| EngineError::ModuleConfigError(error.to_string()))?;
        Ok(self.repository().create_module(&module)?)
    }

    /// 校验并更新 Module 配置。
    ///
    /// # Errors
    /// 配置违反模块不变量或仓储写入失败。
    pub fn update_module(&self, module: ModuleConfig) -> Result<ModuleConfig, EngineError> {
        let module = module
            .validated()
            .map_err(|error| EngineError::ModuleConfigError(error.to_string()))?;
        Ok(self.repository().update_module(&module)?)
    }

    /// 删除 Module 配置。
    ///
    /// # Errors
    /// 模块不存在或仓储删除失败。
    pub fn delete_module(&self, module_id: &str) -> Result<(), EngineError> {
        self.repository()
            .get_module(module_id)?
            .ok_or_else(|| EngineError::Value(format!("module not found: {module_id}")))?;
        Ok(self.repository().delete_module(module_id)?)
    }
}
