//========= Copyright � 1996-2005, Valve Corporation, All rights reserved. ============//
//
// Purpose: 
//
//=====================================================================================//

#include "materialsystem_global.h"
#include "shaderapi/ishaderapi.h"
#include "shadersystem.h"
#if !defined(_PS3) && !defined(OSX)
#include <malloc.h>
#elif defined(OSX)
#include <malloc/malloc.h>
#endif
#include "filesystem.h"

// memdbgon must be the last include file in a .cpp file!!!
#include "tier0/memdbgon.h"

int g_FrameNum;

#ifdef USE_MTL
#else
CShaderAPIBase *g_pShaderAPI = 0;
IShaderAPIDX8 *g_pShaderAPIDX8 = 0;
CShaderDeviceMgrBase* g_pShaderDeviceMgr = 0;
CShaderDeviceBase *g_pShaderDevice = 0;
#endif
IShaderShadow* g_pShaderShadow = 0;

IClientMaterialSystem *g_pClientMaterialSystem = 0;

#if defined( INCLUDE_SCALEFORM )
IScaleformUI* g_pScaleformUI = 0;
#elif defined( INCLUDE_ROCKETUI )
IRocketUI* g_pRocketUI = 0;
#endif
