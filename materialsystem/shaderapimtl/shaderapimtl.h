//===== Copyright  2025, IShaderDeviceMgr, All rights reserved. ======//
//
// Purpose:
//
// $NoKeywords: $
//
//===========================================================================//

#ifndef SHADERAPIMTL_H
#define SHADERAPIMTL_H
#include "shaderapi/ishaderapi.h"
#include "shaderapicommon/meshbase.h"
#include <Metal.hpp>

#include "shaderapi/ishadershadow.h"
#include "shadershadowmtl.h"
#include "materialsystem/IShader.h"


//-----------------------------------------------------------------------------
// State that matters to buffered meshes... (for debugging only)
//-----------------------------------------------------------------------------
struct BufferedState_t
{
	simd::float4x4 m_Transform[3];
	MTL::Viewport m_Viewport;
	int m_BoundTexture[16];
	void *m_VertexShader;
	void *m_PixelShader;
};

//-----------------------------------------------------------------------------
// Compiled lighting state
//-----------------------------------------------------------------------------
struct CompiledLightingState_t
{
	Vector4D	m_AmbientLightCube[6];
	int			m_nLocalLightCount;
	Vector4D	m_PixelShaderLocalLights[6];
	Vector4D	m_VertexShaderLocalLights[20];
	int			m_VertexShaderLocalLightLoopControl[4];
	int			m_VertexShaderLocalLightEnable[VERTEX_SHADER_LIGHT_ENABLE_BOOL_CONST_COUNT];
};

struct InstanceInfo_t
{
	// Have we compiled various bits of lighting state?
	bool		m_bAmbientCubeCompiled : 1;
	bool		m_bPixelShaderLocalLightsCompiled : 1;
	bool		m_bVertexShaderLocalLightsCompiled : 1;

	// Have we set various shader constants?
	bool		m_bSetSkinConstants : 1;
	bool		m_bSetLightVertexShaderConstants : 1;
};

//-----------------------------------------------------------------------------
// The MTL shader API
//-----------------------------------------------------------------------------
// FIXME: Remove this! Either move them into CShaderAPIBase or CShaderAPIDx8
class IShaderAPIMTL : public IShaderAPI
{
public:
	// Draws the mesh
	virtual void DrawMesh( CMeshBase *pMesh, int nCount, const MeshInstanceData_t *pInstances, VertexCompressionType_t nCompressionType, CompiledLightingState_t* pCompiledState, InstanceInfo_t *pInfo ) = 0;

	// Draw the mesh with the currently bound vertex and index buffers.
	virtual void DrawWithVertexAndIndexBuffers( void ) = 0;

	// Gets the current buffered state... (debug only)
	virtual void GetBufferedState( BufferedState_t &state ) = 0;

	// Gets the current backface cull state....
	virtual MTL::CullMode GetCullMode() const = 0;

	// Measures fill rate
	virtual void ComputeFillRate() = 0;

	// Selection mode methods
	virtual bool IsInSelectionMode() const = 0;

	// We hit somefin in selection mode
	virtual void RegisterSelectionHit( float minz, float maxz ) = 0;

	// Get the currently bound material
	virtual IMaterialInternal* GetBoundMaterial() = 0;

	// These methods are called by the transition table
	// They depend on dynamic state so can't exist inside the transition table
	virtual void ApplyZBias( const DepthTestState_t  &shaderState ) = 0;
	virtual void ApplyCullEnable( bool bEnable ) = 0;
	virtual void ApplyFogMode( ShaderFogMode_t fogMode, bool bVertexFog, bool bSRGBWritesEnabled, bool bDisableGammaCorrection ) = 0;

	virtual int GetActualSamplerCount() const = 0;

	virtual bool IsRenderingMesh() const = 0;

	// Fog methods...
	virtual void EnableFixedFunctionFog( bool bFogEnable ) = 0;

	virtual int GetCurrentFrameCounter( void ) const = 0;

	// Workaround hack for visualization of selection mode
	virtual void SetupSelectionModeVisualizationState() = 0;

	virtual bool UsingSoftwareVertexProcessing() const = 0;

	//notification that the SRGB write state is being changed
	virtual void EnabledSRGBWrite( bool bEnabled ) = 0;
	virtual void SetSRGBWrite( bool bState ) = 0;

	// Alpha to coverage
	virtual void ApplyAlphaToCoverage( bool bEnable ) = 0;

	virtual void PrintfVA( char *fmt, va_list vargs ) = 0;
	virtual void Printf( char *fmt, ... ) = 0;
	virtual float Knob( char *knobname, float *setvalue = NULL ) = 0;

	virtual void NotifyShaderConstantsChangedInRenderPass() = 0;

	virtual void GenerateNonInstanceRenderState( MeshInstanceData_t *pInstance, CompiledLightingState_t** pCompiledState, InstanceInfo_t **pInfo ) = 0;

	// Executes the per-instance command buffer
	virtual void ExecuteInstanceCommandBuffer( const unsigned char *pCmdBuf, int nInstanceIndex, bool bForceStateSet ) = 0;

	// Sets the vertex decl
	virtual void SetVertexDecl( VertexFormat_t vertexFormat, bool bHasColorMesh, bool bUsingFlex, bool bUsingMorph, bool bUsingPreTessPatch, VertexStreamSpec_t *pStreamSpec ) = 0;

	// Set Tessellation Enable
#if ENABLE_TESSELLATION
	virtual void SetTessellationMode( TessellationMode_t mode ) = 0;
#else
	void SetTessellationMode( TessellationMode_t mode ) {}
#endif

	virtual void AddShaderComboInformation( const ShaderComboSemantics_t *pSemantics ) = 0;

	virtual float GetLightMapScaleFactor() const = 0;
};

#endif //SHADERAPIMTL_H