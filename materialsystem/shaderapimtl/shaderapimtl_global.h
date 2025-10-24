//===== Copyright  2025, IShaderDeviceMgr, All rights reserved. ======//
//
// Purpose:
//
// $NoKeywords: $
//
//===========================================================================//

#ifndef SHADERAPIMTL_GLOBAL_H
#define SHADERAPIMTL_GLOBAL_H

//-----------------------------------------------------------------------------
// Forward declarations
//-----------------------------------------------------------------------------
class IShaderUtil;
class IVertexBufferMTL;
class IShaderShadowMTL;
class IMeshMgr;
class IShaderAPIMTL;
class IFileSystem;
class IShaderManager;

//-----------------------------------------------------------------------------
// The main shader API
//-----------------------------------------------------------------------------
extern IShaderAPIMTL *g_pShaderAPIMTL;
inline IShaderAPIMTL* ShaderAPI()
{
    return g_pShaderAPIMTL;
}

//-----------------------------------------------------------------------------
// The shader shadow
//-----------------------------------------------------------------------------
IShaderShadowMTL* ShaderShadow();

//-----------------------------------------------------------------------------
// Manager of all vertex + pixel shaders
//-----------------------------------------------------------------------------
inline IShaderManager *ShaderManager()
{
    extern IShaderManager *g_pShaderManager;
    return g_pShaderManager;
}

//-----------------------------------------------------------------------------
// The mesh manager
//-----------------------------------------------------------------------------
IMeshMgr* MeshMgr();

//-----------------------------------------------------------------------------
// The main hardware config interface
//-----------------------------------------------------------------------------
inline IMaterialSystemHardwareConfig* HardwareConfig()
{
    return g_pMaterialSystemHardwareConfig;
}


typedef intp VertexShader_t;
typedef intp PixelShader_t;

#endif // SHADERAPIMTL_GLOBAL_H